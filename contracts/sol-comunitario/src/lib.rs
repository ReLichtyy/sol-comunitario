#![no_std]
//! # Sol Comunitario ☀️
//!
//! Escrow de "pago por energía generada" para paneles solares comunitarios
//! en Costa Rica (salón comunal, escuela, ASADA, EBAIS...).
//!
//! 1. Un **comité de vecinos** (ej. 3 miembros, 2 firmas) y el **instalador**
//!    acuerdan un proyecto: kWh prometidos, % de adelanto, plazo de
//!    instalación y ventana de medición.
//! 2. Los **vecinos** aportan. La plata queda bloqueada en el contrato.
//! 3. Cuando la mayoría del comité confirma la instalación (y la lectura
//!    inicial del medidor), el instalador recibe el adelanto.
//! 4. Durante la ventana, la mayoría del comité firma lecturas ABSOLUTAS del
//!    medidor. El medidor nunca puede retroceder: imposible contar doble.
//! 5. Al terminar la medición hay OBJECTION_DAYS para que un vecino que
//!    aportó objete (`dispute`); el comité fija la lectura real por mayoría
//!    (`resolve_dispute`) y se liquida con ella.
//! 6. Sin objeciones (`settle`, lo llama cualquiera) el instalador cobra
//!    `total * min(generado, prometido) / prometido - adelanto`.
//! 7. Lo que sobra vuelve a los vecinos, proporcional a su aporte.
//!    Si nunca se instala a tiempo, cada vecino recupera el 100%.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, token, Address, Env, String,
    Vec,
};

// Metadatos SEP-46: quedan grabados en el WASM y cualquiera los lee con
// `stellar contract info meta`.
soroban_sdk::contractmeta!(key = "name", val = "Sol Comunitario");
soroban_sdk::contractmeta!(key = "version", val = "0.3.0");
soroban_sdk::contractmeta!(key = "description", val = "Escrow solar comunitario: pago por kWh generado, comite 2 de 3, objecion y credenciales rotables");
soroban_sdk::contractmeta!(key = "source_repo", val = "https://github.com/ReLichtyy/sol-comunitario");

/// Segundos en un día.
pub const DAY: u64 = 86_400;
/// Adelanto máximo permitido: 50% (en puntos base).
pub const MAX_ADVANCE_BPS: u32 = 5_000;
pub const BPS: i128 = 10_000;
/// Largo máximo del nombre del proyecto (bytes).
pub const MAX_NAME_LEN: u32 = 64;
/// Máximo de planes que la empresa puede ofrecer por proyecto.
pub const MAX_PLANS: u32 = 5;
/// Días, después de la medición, en que un vecino puede objetar las lecturas.
pub const OBJECTION_DAYS: u64 = 7;

const BUMP_LOW: u32 = 100_000;
const BUMP_HIGH: u32 = 1_000_000;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    ProjectNotFound = 3,
    InvalidAmount = 4,
    InvalidPlan = 5,
    InvalidAdvance = 6,
    InvalidDays = 7,
    NotFunding = 8,
    InstallDeadlinePassed = 9,
    NoFunds = 10,
    NotMeasuring = 11,
    MeasurementClosed = 12,
    MeasurementStillOpen = 13,
    NothingToClaim = 14,
    RefundNotAvailable = 15,
    InvalidCommittee = 16,
    NotCommitteeMember = 17,
    AlreadyApproved = 18,
    MeterNotIncreasing = 19,
    NameTooLong = 20,
    InObjectionPeriod = 21,
    InDispute = 22,
    NotDisputable = 23,
    NotContributor = 24,
    NotDisputed = 25,
    PlanNotFound = 26,
    AlreadySubscribed = 27,
}

#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Status {
    /// Recibiendo aportes de los vecinos.
    Funding = 0,
    /// Paneles instalados, midiendo energía.
    Measuring = 1,
    /// Liquidado: instalador pagado, vecinos reclaman el sobrante.
    Settled = 2,
    /// Un vecino objetó las lecturas; el comité debe fijar la lectura real.
    Disputed = 3,
}

/// Credencial on-chain de un miembro del comité (estilo DID).
///
/// On-chain va solo lo que el contrato necesita para autorizar: la cuenta
/// (G… o C…), el rol público y desde cuándo está vigente. El DID
/// (`did:pkh:stellar:<red>:<cuenta>`) se deriva de `account` fuera de la
/// cadena; nombre, cédula o teléfono NUNCA se guardan aquí.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Member {
    pub account: Address,
    /// Ej. "Presidenta ADI".
    pub role: String,
    /// Timestamp desde el que esta llave es válida (lo fija el contrato).
    pub since: u64,
}

/// Plan que la empresa ofrece a las familias (ej. "Familiar · 2 paneles").
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Plan {
    pub name: String,
    pub panels: u32,
    /// Precio que paga la familia al firmar.
    pub price: i128,
    /// kWh que la empresa promete entregar a esta familia en la ventana de medición.
    pub kwh: i128,
}

/// Lo que firmó una familia.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Subscription {
    pub plan: u32,
    pub paid: i128,
    pub kwh: i128,
}

/// La parte de energía de una familia en un proyecto.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Share {
    pub plan: u32,
    pub paid: i128,
    /// kWh prometidos por su plan.
    pub kwh_promised: i128,
    /// Su parte de lo generado: generado × (sus kWh / kWh prometidos del proyecto).
    pub kwh_received: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    pub committee: Vec<Member>,
    pub threshold: u32,
    pub token: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Project {
    pub installer: Address,
    /// Ej. "Salón comunal de Santa Teresa".
    pub name: String,
    /// kWh prometidos: suma de los planes firmados por las familias.
    pub kwh_promised: i128,
    /// Adelanto al instalar, en puntos base (3000 = 30%).
    pub advance_bps: u32,
    /// Fecha límite para instalar; si pasa, los vecinos recuperan todo.
    pub install_deadline: u64,
    /// Duración de la ventana de medición (segundos).
    pub measure_secs: u64,
    /// Fin de la ventana de medición (0 si aún no se instala).
    pub measure_end: u64,
    /// Lectura del medidor al instalar.
    pub meter_start: i128,
    /// kWh generados = última lectura aprobada - meter_start.
    pub kwh_generated: i128,
    /// Total aportado por los vecinos.
    pub total: i128,
    /// Total pagado al instalador (adelanto + liquidación).
    pub paid_to_installer: i128,
    pub status: Status,
}

/// Acción que el comité debe aprobar por mayoría.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    /// Confirmar instalación con la lectura inicial del medidor.
    Install(i128),
    /// Lectura absoluta del medidor.
    Reading(i128),
    /// Lectura corregida que resuelve una disputa.
    Resolve(i128),
    /// Cambiar la llave de un miembro (vieja, nueva), conservando su rol.
    Rotate(Address, Address),
    /// Sacar a un miembro del comité.
    Revoke(Address),
}

/// Las aprobaciones de gobierno del comité no pertenecen a ningún proyecto.
const GOVERNANCE: u32 = u32::MAX;

#[contracttype]
#[derive(Clone)]
enum DataKey {
    Config,
    ProjectCount,
    Project(u32),
    Plans(u32),
    /// Suscripción de una familia a un proyecto.
    Contribution(u32, Address),
    Approvals(u32, Action),
}

/// Fase real del proyecto: combina el estado guardado con el reloj.
#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Funding,
    /// Nunca se instaló y pasó el plazo.
    Expired,
    Measuring,
    /// Medición terminada: los vecinos pueden objetar durante OBJECTION_DAYS.
    Objection,
    Disputed,
    /// Sin objeciones: listo para liquidar.
    Ready,
    Settled,
}

#[contract]
pub struct SolComunitario;

#[contractimpl]
impl SolComunitario {
    /// Configura el comité (credenciales + firmas necesarias) y el token de pago.
    pub fn initialize(
        env: Env,
        committee: Vec<Member>,
        threshold: u32,
        token: Address,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Config) {
            return Err(Error::AlreadyInitialized);
        }
        if threshold == 0 || threshold > committee.len() {
            return Err(Error::InvalidCommittee);
        }
        let now = env.ledger().timestamp();
        let mut members = Vec::new(&env);
        for (i, m) in committee.iter().enumerate() {
            if committee.iter().skip(i + 1).any(|o| o.account == m.account) {
                return Err(Error::InvalidCommittee);
            }
            m.account.require_auth();
            members.push_back(Member { since: now, ..m });
        }
        Self::save_config(&env, &Config { committee: members, threshold, token });
        Ok(())
    }

    /// Un miembro firma el cambio de llave de `old` a `new` (ej. perdió el
    /// celular). Con mayoría, `new` hereda el rol de `old` y `old` deja de
    /// poder firmar. Devuelve `true` si esta firma completó la mayoría.
    pub fn rotate_member(env: Env, member: Address, old: Address, new: Address) -> Result<bool, Error> {
        let mut cfg = Self::config(&env)?;
        let idx = Self::member_index(&cfg, &old).ok_or(Error::NotCommitteeMember)?;
        if Self::member_index(&cfg, &new).is_some() {
            return Err(Error::InvalidCommittee);
        }
        if !Self::approve(&env, &cfg, &member, GOVERNANCE, Action::Rotate(old.clone(), new.clone()))? {
            return Ok(false);
        }
        let role = cfg.committee.get_unchecked(idx).role;
        let since = env.ledger().timestamp();
        cfg.committee.set(idx, Member { account: new.clone(), role, since });
        Self::save_config(&env, &cfg);

        env.events().publish((symbol_short!("rotar"),), (old, new));
        Ok(true)
    }

    /// Un miembro firma sacar a `target` del comité. No se permite si el
    /// comité quedaría con menos miembros que firmas necesarias.
    /// Devuelve `true` si esta firma completó la mayoría.
    pub fn revoke_member(env: Env, member: Address, target: Address) -> Result<bool, Error> {
        let mut cfg = Self::config(&env)?;
        let idx = Self::member_index(&cfg, &target).ok_or(Error::NotCommitteeMember)?;
        if cfg.committee.len() - 1 < cfg.threshold {
            return Err(Error::InvalidCommittee);
        }
        if !Self::approve(&env, &cfg, &member, GOVERNANCE, Action::Revoke(target.clone()))? {
            return Ok(false);
        }
        cfg.committee.remove(idx);
        Self::save_config(&env, &cfg);

        env.events().publish((symbol_short!("revocar"),), target);
        Ok(true)
    }

    pub fn is_member(env: Env, account: Address) -> bool {
        Self::config(&env)
            .map(|cfg| Self::member_index(&cfg, &account).is_some())
            .unwrap_or(false)
    }

    /// La empresa (instalador) publica un proyecto con sus planes; un miembro
    /// del comité de vecinos co-firma. Devuelve su id.
    pub fn create_project(
        env: Env,
        installer: Address,
        member: Address,
        name: String,
        plans: Vec<Plan>,
        advance_bps: u32,
        install_days: u32,
        measure_days: u32,
    ) -> Result<u32, Error> {
        Self::require_member(&Self::config(&env)?, &member)?;
        installer.require_auth();
        member.require_auth();

        if name.len() > MAX_NAME_LEN {
            return Err(Error::NameTooLong);
        }
        if plans.is_empty() || plans.len() > MAX_PLANS {
            return Err(Error::InvalidPlan);
        }
        for pl in plans.iter() {
            if pl.panels == 0 || pl.price <= 0 || pl.kwh <= 0 || pl.name.len() > MAX_NAME_LEN {
                return Err(Error::InvalidPlan);
            }
        }
        if advance_bps > MAX_ADVANCE_BPS {
            return Err(Error::InvalidAdvance);
        }
        if install_days == 0 || measure_days == 0 {
            return Err(Error::InvalidDays);
        }

        let project = Project {
            installer: installer.clone(),
            name,
            kwh_promised: 0,
            advance_bps,
            install_deadline: env.ledger().timestamp() + install_days as u64 * DAY,
            measure_secs: measure_days as u64 * DAY,
            measure_end: 0,
            meter_start: 0,
            kwh_generated: 0,
            total: 0,
            paid_to_installer: 0,
            status: Status::Funding,
        };

        let id = Self::project_count(env.clone());
        env.storage().instance().set(&DataKey::ProjectCount, &(id + 1));
        Self::save(&env, id, &project);
        Self::put(&env, &DataKey::Plans(id), &plans);

        env.events()
            .publish((symbol_short!("project"), id), (installer, plans.len()));
        Ok(id)
    }

    /// Una familia firma un plan: paga su precio (queda bloqueado en el
    /// contrato) y la promesa de energía del proyecto crece con su plan.
    pub fn subscribe(env: Env, neighbor: Address, id: u32, plan: u32) -> Result<(), Error> {
        neighbor.require_auth();
        let mut p = Self::load(&env, id)?;
        Self::require_funding(&env, &p)?;
        let chosen = Self::plans(&env, id)?.get(plan).ok_or(Error::PlanNotFound)?;
        let key = DataKey::Contribution(id, neighbor.clone());
        if env.storage().persistent().has(&key) {
            return Err(Error::AlreadySubscribed);
        }

        let cfg = Self::config(&env)?;
        token::Client::new(&env, &cfg.token).transfer(
            &neighbor,
            &env.current_contract_address(),
            &chosen.price,
        );
        Self::put(&env, &key, &Subscription { plan, paid: chosen.price, kwh: chosen.kwh });

        p.total += chosen.price;
        p.kwh_promised += chosen.kwh;
        Self::save(&env, id, &p);

        env.events()
            .publish((symbol_short!("firma"), id), (neighbor, plan));
        Ok(())
    }

    /// Un miembro del comité confirma la instalación con la lectura inicial
    /// del medidor. Con la mayoría de firmas se paga el adelanto y empieza la
    /// medición. Devuelve `true` si esta firma completó la mayoría.
    pub fn approve_installation(
        env: Env,
        member: Address,
        id: u32,
        meter_start: i128,
    ) -> Result<bool, Error> {
        let cfg = Self::config(&env)?;
        let mut p = Self::load(&env, id)?;
        Self::require_funding(&env, &p)?;
        if p.total == 0 {
            return Err(Error::NoFunds);
        }
        if meter_start < 0 {
            return Err(Error::InvalidAmount);
        }
        if !Self::approve(&env, &cfg, &member, id, Action::Install(meter_start))? {
            return Ok(false);
        }

        let advance = p.total * p.advance_bps as i128 / BPS;
        Self::pay(&env, &cfg, &p.installer, advance);
        p.paid_to_installer = advance;
        p.meter_start = meter_start;
        p.measure_end = env.ledger().timestamp() + p.measure_secs;
        p.status = Status::Measuring;
        Self::save(&env, id, &p);

        env.events()
            .publish((symbol_short!("install"), id), (meter_start, advance));
        Ok(true)
    }

    /// Un miembro del comité firma una lectura ABSOLUTA del medidor.
    /// Se aplica cuando la mayoría firma el mismo valor.
    /// Devuelve `true` si esta firma completó la mayoría.
    pub fn submit_reading(env: Env, member: Address, id: u32, meter: i128) -> Result<bool, Error> {
        let cfg = Self::config(&env)?;
        let mut p = Self::load(&env, id)?;
        match Self::phase(&env, &p) {
            Phase::Measuring => {}
            Phase::Objection | Phase::Disputed | Phase::Ready => {
                return Err(Error::MeasurementClosed)
            }
            _ => return Err(Error::NotMeasuring),
        }
        if meter <= p.meter_start + p.kwh_generated {
            return Err(Error::MeterNotIncreasing);
        }
        if !Self::approve(&env, &cfg, &member, id, Action::Reading(meter))? {
            return Ok(false);
        }

        p.kwh_generated = meter - p.meter_start;
        Self::save(&env, id, &p);

        env.events()
            .publish((symbol_short!("lectura"), id), (meter, p.kwh_generated));
        Ok(true)
    }

    /// Un vecino que aportó objeta las lecturas durante el período de
    /// objeción. El proyecto queda congelado hasta que el comité lo resuelva.
    pub fn dispute(env: Env, neighbor: Address, id: u32) -> Result<(), Error> {
        neighbor.require_auth();
        let mut p = Self::load(&env, id)?;
        if Self::contribution(&env, &DataKey::Contribution(id, neighbor.clone())) == 0 {
            return Err(Error::NotContributor);
        }
        if Self::phase(&env, &p) != Phase::Objection {
            return Err(Error::NotDisputable);
        }
        p.status = Status::Disputed;
        Self::save(&env, id, &p);

        env.events()
            .publish((symbol_short!("disputa"), id), (neighbor, p.kwh_generated));
        Ok(())
    }

    /// Un miembro del comité firma la lectura correcta del medidor para
    /// resolver una disputa (puede ser menor a la reportada). Con mayoría,
    /// se corrige y se liquida en el acto.
    /// Devuelve `true` si esta firma completó la mayoría.
    pub fn resolve_dispute(env: Env, member: Address, id: u32, meter: i128) -> Result<bool, Error> {
        let cfg = Self::config(&env)?;
        let mut p = Self::load(&env, id)?;
        if p.status != Status::Disputed {
            return Err(Error::NotDisputed);
        }
        Self::require_member(&cfg, &member)?;
        if meter < p.meter_start {
            return Err(Error::InvalidAmount);
        }
        if !Self::approve(&env, &cfg, &member, id, Action::Resolve(meter))? {
            return Ok(false);
        }
        p.kwh_generated = meter - p.meter_start;
        Self::finish(&env, &cfg, id, &mut p);
        Ok(true)
    }

    /// Cierra el proyecto cuando terminó la medición y el período de
    /// objeción sin disputas. Cualquiera puede llamarlo, así nadie puede
    /// "secuestrar" los fondos no llamando.
    /// Devuelve el pago final al instalador.
    pub fn settle(env: Env, id: u32) -> Result<i128, Error> {
        let mut p = Self::load(&env, id)?;
        match Self::phase(&env, &p) {
            Phase::Ready => {}
            Phase::Measuring => return Err(Error::MeasurementStillOpen),
            Phase::Objection => return Err(Error::InObjectionPeriod),
            Phase::Disputed => return Err(Error::InDispute),
            _ => return Err(Error::NotMeasuring),
        }
        Ok(Self::finish(&env, &Self::config(&env)?, id, &mut p))
    }

    /// Un vecino recupera su parte de lo que no se pagó al instalador:
    /// `aporte * (total - pagado) / total`. Disponible al liquidar o si el
    /// proyecto nunca se instaló a tiempo (en ese caso es el 100%).
    pub fn claim_refund(env: Env, neighbor: Address, id: u32) -> Result<i128, Error> {
        neighbor.require_auth();
        let key = DataKey::Contribution(id, neighbor.clone());
        let contribution = Self::contribution(&env, &key);
        if contribution == 0 {
            return Err(Error::NothingToClaim);
        }
        let p = Self::load(&env, id)?;
        if !matches!(Self::phase(&env, &p), Phase::Settled | Phase::Expired) {
            return Err(Error::RefundNotAvailable);
        }

        let refund = contribution * (p.total - p.paid_to_installer) / p.total;
        // Primero borrar (evita doble reclamo), luego transferir.
        env.storage().persistent().remove(&key);
        Self::pay(&env, &Self::config(&env)?, &neighbor, refund);

        env.events()
            .publish((symbol_short!("refund"), id), (neighbor, refund));
        Ok(refund)
    }

    // ---------- lecturas ----------

    pub fn get_config(env: Env) -> Result<Config, Error> {
        Self::config(&env)
    }

    pub fn get_project(env: Env, id: u32) -> Result<Project, Error> {
        Self::load(&env, id)
    }

    pub fn get_plans(env: Env, id: u32) -> Result<Vec<Plan>, Error> {
        Self::plans(&env, id)
    }

    /// La parte de energía que le toca a una familia.
    pub fn get_share(env: Env, id: u32, neighbor: Address) -> Result<Share, Error> {
        let p = Self::load(&env, id)?;
        let sub: Subscription = env
            .storage()
            .persistent()
            .get(&DataKey::Contribution(id, neighbor))
            .ok_or(Error::NotContributor)?;
        Ok(Share {
            plan: sub.plan,
            paid: sub.paid,
            kwh_promised: sub.kwh,
            kwh_received: p.kwh_generated * sub.kwh / p.kwh_promised,
        })
    }

    pub fn get_contribution(env: Env, id: u32, neighbor: Address) -> i128 {
        Self::contribution(&env, &DataKey::Contribution(id, neighbor))
    }

    pub fn project_count(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::ProjectCount)
            .unwrap_or(0)
    }

    /// Desempeño en puntos base: generado / prometido (tope 10000 = 100%).
    pub fn performance_bps(env: Env, id: u32) -> Result<u32, Error> {
        let p = Self::load(&env, id)?;
        if p.kwh_promised == 0 {
            return Ok(0);
        }
        Ok((p.kwh_generated.min(p.kwh_promised) * BPS / p.kwh_promised) as u32)
    }

    // ---------- helpers ----------

    fn phase(env: &Env, p: &Project) -> Phase {
        let now = env.ledger().timestamp();
        match p.status {
            Status::Funding if now > p.install_deadline => Phase::Expired,
            Status::Funding => Phase::Funding,
            Status::Measuring if now <= p.measure_end => Phase::Measuring,
            Status::Measuring if now <= p.measure_end + OBJECTION_DAYS * DAY => Phase::Objection,
            Status::Measuring => Phase::Ready,
            Status::Disputed => Phase::Disputed,
            Status::Settled => Phase::Settled,
        }
    }

    /// Paga al instalador lo ganado y marca el proyecto como liquidado.
    fn finish(env: &Env, cfg: &Config, id: u32, p: &mut Project) -> i128 {
        let delivered = p.kwh_generated.min(p.kwh_promised);
        let earned = p.total * delivered / p.kwh_promised;
        let final_pay = (earned - p.paid_to_installer).max(0);

        Self::pay(env, cfg, &p.installer, final_pay);
        p.paid_to_installer += final_pay;
        p.status = Status::Settled;
        Self::save(env, id, p);

        env.events().publish(
            (symbol_short!("settle"), id),
            (final_pay, p.total - p.paid_to_installer),
        );
        final_pay
    }

    fn require_funding(env: &Env, p: &Project) -> Result<(), Error> {
        match Self::phase(env, p) {
            Phase::Funding => Ok(()),
            Phase::Expired => Err(Error::InstallDeadlinePassed),
            _ => Err(Error::NotFunding),
        }
    }

    fn require_member(cfg: &Config, member: &Address) -> Result<(), Error> {
        Self::member_index(cfg, member)
            .map(|_| ())
            .ok_or(Error::NotCommitteeMember)
    }

    fn member_index(cfg: &Config, account: &Address) -> Option<u32> {
        cfg.committee
            .iter()
            .position(|m| m.account == *account)
            .map(|i| i as u32)
    }

    fn save_config(env: &Env, cfg: &Config) {
        let st = env.storage().instance();
        st.set(&DataKey::Config, cfg);
        st.extend_ttl(BUMP_LOW, BUMP_HIGH);
    }

    /// Registra la firma de `member` para `action`. Devuelve `true` cuando se
    /// alcanza la mayoría (y limpia las firmas de esa acción).
    fn approve(
        env: &Env,
        cfg: &Config,
        member: &Address,
        id: u32,
        action: Action,
    ) -> Result<bool, Error> {
        Self::require_member(cfg, member)?;
        member.require_auth();

        let key = DataKey::Approvals(id, action);
        let st = env.storage().persistent();
        let mut signers: Vec<Address> = st.get(&key).unwrap_or(Vec::new(env));
        if signers.contains(member) {
            return Err(Error::AlreadyApproved);
        }
        signers.push_back(member.clone());

        if signers.len() >= cfg.threshold {
            st.remove(&key);
            return Ok(true);
        }
        Self::put(env, &key, &signers);
        Ok(false)
    }

    /// Paga desde el contrato; ignora montos en cero.
    fn pay(env: &Env, cfg: &Config, to: &Address, amount: i128) {
        if amount > 0 {
            token::Client::new(env, &cfg.token).transfer(
                &env.current_contract_address(),
                to,
                &amount,
            );
        }
    }

    fn config(env: &Env) -> Result<Config, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(Error::NotInitialized)
    }

    /// Lo que pagó una familia (0 si no firmó).
    fn contribution(env: &Env, key: &DataKey) -> i128 {
        env.storage()
            .persistent()
            .get::<_, Subscription>(key)
            .map(|s| s.paid)
            .unwrap_or(0)
    }

    fn plans(env: &Env, id: u32) -> Result<Vec<Plan>, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Plans(id))
            .ok_or(Error::ProjectNotFound)
    }

    fn load(env: &Env, id: u32) -> Result<Project, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Project(id))
            .ok_or(Error::ProjectNotFound)
    }

    fn save(env: &Env, id: u32, p: &Project) {
        Self::put(env, &DataKey::Project(id), p);
    }

    /// Guarda en almacenamiento persistente y extiende su TTL.
    fn put<V>(env: &Env, key: &DataKey, v: &V)
    where
        V: soroban_sdk::IntoVal<Env, soroban_sdk::Val>,
    {
        let st = env.storage().persistent();
        st.set(key, v);
        st.extend_ttl(key, BUMP_LOW, BUMP_HIGH);
    }
}

#[cfg(test)]
mod test;
