#![cfg(test)]
extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    vec, Address, Env, String, Vec,
};

struct Setup<'a> {
    env: Env,
    client: SolComunitarioClient<'a>,
    token: TokenClient<'a>,
    /// La empresa Sol Comunitario (instala y vende los planes).
    installer: Address,
    /// Comité de vecinos: 3 miembros, se necesitan 2 firmas.
    c1: Address,
    c2: Address,
    c3: Address,
    ana: Address,
    beto: Address,
}

fn setup() -> Setup<'static> {
    let env = Env::default();
    env.mock_all_auths();

    let [installer, c1, c2, c3, ana, beto, issuer] =
        core::array::from_fn(|_| Address::generate(&env));

    let sac = env.register_stellar_asset_contract_v2(issuer);
    let minter = StellarAssetClient::new(&env, &sac.address());
    minter.mint(&ana, &100_000);
    minter.mint(&beto, &100_000);

    let contract_id = env.register(SolComunitario, ());
    let client = SolComunitarioClient::new(&env, &contract_id);
    client.initialize(
        &vec![
            &env,
            m(&env, &c1, "Presidenta ADI"),
            m(&env, &c2, "Fontanero ASADA"),
            m(&env, &c3, "Maestra de la escuela"),
        ],
        &2,
        &sac.address(),
    );

    let token = TokenClient::new(&env, &sac.address());
    Setup { env, client, token, installer, c1, c2, c3, ana, beto }
}

/// Credencial de un miembro del comité (`since` lo pone el contrato).
fn m(env: &Env, account: &Address, role: &str) -> Member {
    Member { account: account.clone(), role: String::from_str(env, role), since: 0 }
}

fn plan(env: &Env, name: &str, panels: u32, price: i128, kwh: i128) -> Plan {
    Plan { name: String::from_str(env, name), panels, price, kwh }
}

/// Planes típicos: kWh prometidos para los 90 días de medición.
const BASICO: u32 = 0; // 1 panel  · 2 500 · 250 kWh
const FAMILIAR: u32 = 1; // 2 paneles · 5 000 · 500 kWh
const COMERCIO: u32 = 2; // 4 paneles · 10 000 · 1 000 kWh

fn default_plans(env: &Env) -> Vec<Plan> {
    vec![
        env,
        plan(env, "Basico", 1, 2_500, 250),
        plan(env, "Familiar", 2, 5_000, 500),
        plan(env, "Comercio", 4, 10_000, 1_000),
    ]
}

fn new_project_with(s: &Setup, plans: &Vec<Plan>, advance_bps: u32) -> u32 {
    s.client.create_project(
        &s.installer,
        &s.c1,
        &String::from_str(&s.env, "Santa Teresa Solar"),
        plans,
        &advance_bps,
        &30,
        &90,
    )
}

/// Proyecto típico: 3 planes, 30% de adelanto, 30 días para instalar, 90 de medición.
fn new_project(s: &Setup) -> u32 {
    new_project_with(s, &default_plans(&s.env), 3_000)
}

/// c1 + c2 confirman la instalación con el medidor en `meter_start`.
fn install(s: &Setup, id: u32, meter_start: i128) {
    assert!(!s.client.approve_installation(&s.c1, &id, &meter_start));
    assert!(s.client.approve_installation(&s.c2, &id, &meter_start));
}

/// c1 + c2 firman la misma lectura absoluta del medidor.
fn read(s: &Setup, id: u32, meter: i128) {
    assert!(!s.client.submit_reading(&s.c1, &id, &meter));
    assert!(s.client.submit_reading(&s.c2, &id, &meter));
}

fn advance_days(env: &Env, days: u64) {
    env.ledger().with_mut(|l| l.timestamp += days * DAY);
}

// ---------------- camino feliz ----------------

#[test]
fn camino_feliz_80_por_ciento_de_la_energia() {
    let s = setup();
    let id = new_project(&s);
    s.client.subscribe(&s.ana, &id, &FAMILIAR); // 5 000 · 500 kWh
    s.client.subscribe(&s.beto, &id, &BASICO); // 2 500 · 250 kWh
    let p = s.client.get_project(&id);
    assert_eq!((p.total, p.kwh_promised), (7_500, 750));

    advance_days(&s.env, 10);
    install(&s, id, 5_000);
    assert_eq!(s.token.balance(&s.installer), 2_250); // 30% de adelanto

    // Lecturas mensuales ABSOLUTAS: termina en 5 600 => 600 kWh (80%).
    for meter in [5_180, 5_400, 5_600] {
        advance_days(&s.env, 30);
        read(&s, id, meter);
    }
    assert_eq!(s.client.performance_bps(&id), 8_000);

    // Cada familia recibe energía según su plan.
    let ana = s.client.get_share(&id, &s.ana);
    assert_eq!((ana.plan, ana.kwh_promised, ana.kwh_received), (FAMILIAR, 500, 400));
    assert_eq!(s.client.get_share(&id, &s.beto).kwh_received, 200);

    advance_days(&s.env, 1 + OBJECTION_DAYS);
    // Ganó 80% de 7 500 = 6 000; ya recibió 2 250 => paga 3 750.
    assert_eq!(s.client.settle(&id), 3_750);
    assert_eq!(s.token.balance(&s.installer), 6_000);

    // Sobran 1 500 => proporcional a lo pagado.
    assert_eq!(s.client.claim_refund(&s.ana, &id), 1_000);
    assert_eq!(s.client.claim_refund(&s.beto, &id), 500);
    assert_eq!(s.token.balance(&s.client.address), 0);
}

// ---------------- planes y suscripción ----------------

#[test]
fn familias_firman_un_plan_y_la_promesa_crece() {
    let s = setup();
    let id = new_project(&s);
    assert_eq!(s.client.get_plans(&id).len(), 3);

    s.client.subscribe(&s.ana, &id, &COMERCIO);
    assert_eq!(s.client.get_project(&id).kwh_promised, 1_000);
    assert_eq!(s.client.get_contribution(&id, &s.ana), 10_000);
    assert_eq!(s.token.balance(&s.ana), 90_000);

    assert_eq!(
        s.client.try_subscribe(&s.ana, &id, &BASICO),
        Err(Ok(Error::AlreadySubscribed))
    );
    assert_eq!(
        s.client.try_subscribe(&s.beto, &id, &9),
        Err(Ok(Error::PlanNotFound))
    );
    assert_eq!(
        s.client.try_get_share(&id, &s.beto),
        Err(Ok(Error::NotContributor))
    );
}

// ---------------- comité 2 de 3 ----------------

#[test]
fn una_sola_firma_no_libera_el_adelanto() {
    let s = setup();
    let id = new_project(&s);
    s.client.subscribe(&s.ana, &id, &COMERCIO);

    assert!(!s.client.approve_installation(&s.c1, &id, &0));
    assert_eq!(s.client.get_project(&id).status, Status::Funding);
    assert_eq!(s.token.balance(&s.installer), 0);

    assert!(s.client.approve_installation(&s.c3, &id, &0));
    assert_eq!(s.client.get_project(&id).status, Status::Measuring);
    assert_eq!(s.token.balance(&s.installer), 3_000);
}

#[test]
fn nadie_firma_dos_veces() {
    let s = setup();
    let id = new_project(&s);
    s.client.subscribe(&s.ana, &id, &BASICO);
    s.client.approve_installation(&s.c1, &id, &0);
    assert_eq!(
        s.client.try_approve_installation(&s.c1, &id, &0),
        Err(Ok(Error::AlreadyApproved))
    );
}

#[test]
fn extranos_no_pueden_firmar() {
    let s = setup();
    let id = new_project(&s);
    s.client.subscribe(&s.ana, &id, &BASICO);
    assert_eq!(
        s.client.try_approve_installation(&s.installer, &id, &0),
        Err(Ok(Error::NotCommitteeMember))
    );
    assert_eq!(
        s.client.try_create_project(
            &s.installer,
            &s.ana,
            &String::from_str(&s.env, "x"),
            &default_plans(&s.env),
            &0,
            &30,
            &90
        ),
        Err(Ok(Error::NotCommitteeMember))
    );
}

#[test]
fn lecturas_en_desacuerdo_no_cuentan_hasta_tener_mayoria() {
    let s = setup();
    let id = new_project(&s);
    s.client.subscribe(&s.ana, &id, &COMERCIO);
    install(&s, id, 0);

    assert!(!s.client.submit_reading(&s.c1, &id, &300));
    assert!(!s.client.submit_reading(&s.c2, &id, &250));
    assert_eq!(s.client.get_project(&id).kwh_generated, 0);

    assert!(s.client.submit_reading(&s.c3, &id, &250));
    assert_eq!(s.client.get_project(&id).kwh_generated, 250);
}

#[test]
fn el_medidor_nunca_retrocede() {
    let s = setup();
    let id = new_project(&s);
    s.client.subscribe(&s.ana, &id, &COMERCIO);
    install(&s, id, 5_000);
    read(&s, id, 5_400);

    assert_eq!(
        s.client.try_submit_reading(&s.c1, &id, &5_400),
        Err(Ok(Error::MeterNotIncreasing))
    );
    assert_eq!(
        s.client.try_submit_reading(&s.c1, &id, &4_000),
        Err(Ok(Error::MeterNotIncreasing))
    );
    assert_eq!(s.client.get_project(&id).kwh_generated, 400);
}

// ---------------- pago por resultado ----------------

#[test]
fn sobre_generar_no_paga_mas_del_100() {
    let s = setup();
    let id = new_project(&s);
    s.client.subscribe(&s.ana, &id, &COMERCIO);
    install(&s, id, 0);
    read(&s, id, 1_500);
    advance_days(&s.env, 91 + OBJECTION_DAYS);
    assert_eq!(s.client.settle(&id), 7_000);
    assert_eq!(s.token.balance(&s.installer), 10_000);
    assert_eq!(s.client.performance_bps(&id), 10_000);
    assert_eq!(s.client.claim_refund(&s.ana, &id), 0);
}

#[test]
fn paneles_que_no_generan_empresa_solo_se_queda_el_adelanto() {
    let s = setup();
    let id = new_project(&s);
    s.client.subscribe(&s.ana, &id, &COMERCIO);
    install(&s, id, 0);
    read(&s, id, 100); // 10% < 30% de adelanto
    advance_days(&s.env, 91 + OBJECTION_DAYS);
    assert_eq!(s.client.settle(&id), 0);
    assert_eq!(s.token.balance(&s.installer), 3_000);
    assert_eq!(s.client.claim_refund(&s.ana, &id), 7_000);
}

#[test]
fn nunca_se_instala_familias_recuperan_todo() {
    let s = setup();
    let id = new_project(&s);
    s.client.subscribe(&s.ana, &id, &FAMILIAR);

    assert_eq!(
        s.client.try_claim_refund(&s.ana, &id),
        Err(Ok(Error::RefundNotAvailable))
    );

    advance_days(&s.env, 31);
    assert_eq!(
        s.client.try_approve_installation(&s.c1, &id, &0),
        Err(Ok(Error::InstallDeadlinePassed))
    );
    assert_eq!(
        s.client.try_subscribe(&s.beto, &id, &BASICO),
        Err(Ok(Error::InstallDeadlinePassed))
    );
    assert_eq!(s.client.claim_refund(&s.ana, &id), 5_000);
    assert_eq!(s.token.balance(&s.ana), 100_000);
}

// ---------------- tiempos y estados ----------------

#[test]
fn no_se_liquida_antes_de_tiempo_ni_se_mide_despues() {
    let s = setup();
    let id = new_project(&s);
    s.client.subscribe(&s.ana, &id, &BASICO);
    install(&s, id, 0);

    advance_days(&s.env, 45);
    assert_eq!(
        s.client.try_settle(&id),
        Err(Ok(Error::MeasurementStillOpen))
    );

    advance_days(&s.env, 46);
    assert_eq!(
        s.client.try_submit_reading(&s.c1, &id, &500),
        Err(Ok(Error::MeasurementClosed))
    );
}

#[test]
fn no_doble_liquidacion_ni_doble_reembolso() {
    let s = setup();
    let id = new_project(&s);
    s.client.subscribe(&s.ana, &id, &COMERCIO);
    install(&s, id, 0);
    read(&s, id, 500);
    advance_days(&s.env, 91 + OBJECTION_DAYS);
    assert_eq!(s.client.settle(&id), 2_000);

    assert_eq!(s.client.try_settle(&id), Err(Ok(Error::NotMeasuring)));
    assert_eq!(s.client.claim_refund(&s.ana, &id), 5_000);
    assert_eq!(
        s.client.try_claim_refund(&s.ana, &id),
        Err(Ok(Error::NothingToClaim))
    );
}

#[test]
fn no_se_instala_sin_familias_ni_se_firma_despues_de_instalar() {
    let s = setup();
    let id = new_project(&s);
    assert_eq!(
        s.client.try_approve_installation(&s.c1, &id, &0),
        Err(Ok(Error::NoFunds))
    );
    s.client.subscribe(&s.ana, &id, &BASICO);
    install(&s, id, 0);
    assert_eq!(
        s.client.try_subscribe(&s.beto, &id, &BASICO),
        Err(Ok(Error::NotFunding))
    );
}

#[test]
fn lecturas_solo_durante_la_medicion() {
    let s = setup();
    let id = new_project(&s);
    assert_eq!(
        s.client.try_submit_reading(&s.c1, &id, &100),
        Err(Ok(Error::NotMeasuring))
    );
}

// ---------------- validaciones ----------------

#[test]
fn validaciones_de_creacion() {
    let s = setup();
    let name = String::from_str(&s.env, "Nosara Solar");
    let mk = |plans: &Vec<Plan>, bps: u32, inst: u32| {
        s.client
            .try_create_project(&s.installer, &s.c1, &name, plans, &bps, &inst, &90)
    };
    let ok = default_plans(&s.env);
    assert_eq!(mk(&Vec::new(&s.env), 3_000, 30), Err(Ok(Error::InvalidPlan)));
    assert_eq!(
        mk(&vec![&s.env, plan(&s.env, "x", 1, 0, 100)], 3_000, 30),
        Err(Ok(Error::InvalidPlan))
    );
    assert_eq!(
        mk(&vec![&s.env, plan(&s.env, "x", 0, 100, 100)], 3_000, 30),
        Err(Ok(Error::InvalidPlan))
    );
    let mut six = Vec::new(&s.env);
    for _ in 0..6 {
        six.push_back(plan(&s.env, "x", 1, 1, 1));
    }
    assert_eq!(mk(&six, 3_000, 30), Err(Ok(Error::InvalidPlan)));
    assert_eq!(mk(&ok, 6_000, 30), Err(Ok(Error::InvalidAdvance)));
    assert_eq!(mk(&ok, 3_000, 0), Err(Ok(Error::InvalidDays)));

    let long = String::from_str(&s.env, &"x".repeat(MAX_NAME_LEN as usize + 1));
    assert_eq!(
        s.client
            .try_create_project(&s.installer, &s.c1, &long, &ok, &0, &30, &90),
        Err(Ok(Error::NameTooLong))
    );

    assert_eq!(
        s.client.try_subscribe(&s.ana, &42, &BASICO),
        Err(Ok(Error::ProjectNotFound))
    );
    new_project(&s);
    assert_eq!(s.client.project_count(), 1);
}

#[test]
fn comite_invalido_y_doble_inicializacion() {
    let s = setup();
    let other = Address::generate(&s.env);
    assert_eq!(
        s.client
            .try_initialize(&vec![&s.env, m(&s.env, &other, "x")], &1, &other),
        Err(Ok(Error::AlreadyInitialized))
    );

    let env = Env::default();
    env.mock_all_auths();
    let fresh = SolComunitarioClient::new(&env, &env.register(SolComunitario, ()));
    let a = Address::generate(&env);
    let b = Address::generate(&env);
    assert_eq!(
        fresh.try_initialize(&vec![&env, m(&env, &a, "a"), m(&env, &b, "b")], &0, &a),
        Err(Ok(Error::InvalidCommittee))
    );
    assert_eq!(
        fresh.try_initialize(&vec![&env, m(&env, &a, "a"), m(&env, &b, "b")], &3, &a),
        Err(Ok(Error::InvalidCommittee))
    );
    assert_eq!(
        fresh.try_initialize(&vec![&env, m(&env, &a, "a"), m(&env, &a, "a")], &2, &a),
        Err(Ok(Error::InvalidCommittee))
    );
}

#[test]
fn crear_proyecto_requiere_firma_de_empresa_y_miembro() {
    let s = setup();
    new_project(&s);
    let auths = s.env.auths();
    assert_eq!(auths.len(), 2);
    assert!(auths.iter().any(|(a, _)| *a == s.installer));
    assert!(auths.iter().any(|(a, _)| *a == s.c1));
}

#[test]
fn redondeo_nunca_deja_al_contrato_en_negativo() {
    let s = setup();
    let plans = vec![&s.env, plan(&s.env, "A", 1, 1_001, 4), plan(&s.env, "B", 1, 333, 3)];
    let id = new_project_with(&s, &plans, 3_333);
    s.client.subscribe(&s.ana, &id, &0);
    s.client.subscribe(&s.beto, &id, &1);
    install(&s, id, 0);
    read(&s, id, 3);
    advance_days(&s.env, 91 + OBJECTION_DAYS);
    s.client.settle(&id);
    s.client.claim_refund(&s.ana, &id);
    s.client.claim_refund(&s.beto, &id);
    let left = s.token.balance(&s.client.address);
    assert!((0..=2).contains(&left));
}

// ---------------- período de objeción ----------------

/// Lectura inflada: el comité (c1 + c2) firma 1 000 kWh aunque el medidor
/// realmente marcaba 600.
fn inflated_project(s: &Setup) -> u32 {
    let id = new_project(s);
    s.client.subscribe(&s.ana, &id, &COMERCIO);
    install(s, id, 0);
    read(s, id, 1_000);
    id
}

#[test]
fn no_se_liquida_durante_la_objecion() {
    let s = setup();
    let id = inflated_project(&s);
    advance_days(&s.env, 91);
    assert_eq!(
        s.client.try_settle(&id),
        Err(Ok(Error::InObjectionPeriod))
    );
    advance_days(&s.env, OBJECTION_DAYS);
    assert_eq!(s.client.settle(&id), 7_000);
}

#[test]
fn familia_objeta_y_el_comite_corrige_la_lectura() {
    let s = setup();
    let id = inflated_project(&s);
    advance_days(&s.env, 91);

    s.client.dispute(&s.ana, &id);
    assert_eq!(s.client.get_project(&id).status, Status::Disputed);

    advance_days(&s.env, 30);
    assert_eq!(s.client.try_settle(&id), Err(Ok(Error::InDispute)));

    assert!(!s.client.resolve_dispute(&s.c1, &id, &600));
    assert!(s.client.resolve_dispute(&s.c3, &id, &600));

    let p = s.client.get_project(&id);
    assert_eq!(p.status, Status::Settled);
    assert_eq!(p.kwh_generated, 600);
    assert_eq!(s.token.balance(&s.installer), 6_000);
    assert_eq!(s.client.claim_refund(&s.ana, &id), 4_000);
}

#[test]
fn solo_quien_firmo_un_plan_puede_objetar() {
    let s = setup();
    let id = inflated_project(&s);
    advance_days(&s.env, 91);
    assert_eq!(
        s.client.try_dispute(&s.beto, &id),
        Err(Ok(Error::NotContributor))
    );
}

#[test]
fn objecion_solo_dentro_del_plazo() {
    let s = setup();
    let id = inflated_project(&s);
    assert_eq!(
        s.client.try_dispute(&s.ana, &id),
        Err(Ok(Error::NotDisputable))
    );
    advance_days(&s.env, 91 + OBJECTION_DAYS);
    assert_eq!(
        s.client.try_dispute(&s.ana, &id),
        Err(Ok(Error::NotDisputable))
    );
}

#[test]
fn resolver_requiere_disputa_abierta_y_comite() {
    let s = setup();
    let id = inflated_project(&s);
    advance_days(&s.env, 91);
    assert_eq!(
        s.client.try_resolve_dispute(&s.c1, &id, &600),
        Err(Ok(Error::NotDisputed))
    );
    s.client.dispute(&s.ana, &id);
    assert_eq!(
        s.client.try_resolve_dispute(&s.ana, &id, &600),
        Err(Ok(Error::NotCommitteeMember))
    );
    assert_eq!(
        s.client.try_resolve_dispute(&s.c1, &id, &-1),
        Err(Ok(Error::InvalidAmount))
    );
}

// ---------------- identidad del comité (estilo DID) ----------------

#[test]
fn las_credenciales_guardan_rol_y_fecha() {
    let s = setup();
    let cfg = s.client.get_config();
    assert_eq!(cfg.committee.len(), 3);
    let marta = cfg.committee.get(0).unwrap();
    assert_eq!(marta.account, s.c1);
    assert_eq!(marta.role, String::from_str(&s.env, "Presidenta ADI"));
    assert!(s.client.is_member(&s.c2));
    assert!(!s.client.is_member(&s.ana));
}

#[test]
fn luis_pierde_el_celular_y_rota_su_llave() {
    let s = setup();
    let luis_nuevo = Address::generate(&s.env);
    advance_days(&s.env, 5);

    assert!(!s.client.rotate_member(&s.c1, &s.c2, &luis_nuevo));
    assert!(s.client.rotate_member(&s.c3, &s.c2, &luis_nuevo));

    let cfg = s.client.get_config();
    let luis = cfg.committee.iter().find(|m| m.account == luis_nuevo).unwrap();
    assert_eq!(luis.role, String::from_str(&s.env, "Fontanero ASADA"));
    assert_eq!(luis.since, 5 * DAY);
    assert!(!s.client.is_member(&s.c2));

    let id = new_project(&s);
    s.client.subscribe(&s.ana, &id, &BASICO);
    assert_eq!(
        s.client.try_approve_installation(&s.c2, &id, &0),
        Err(Ok(Error::NotCommitteeMember))
    );
    assert!(!s.client.approve_installation(&luis_nuevo, &id, &0));
    assert!(s.client.approve_installation(&s.c1, &id, &0));
}

#[test]
fn rotar_valida_miembros() {
    let s = setup();
    let x = Address::generate(&s.env);
    assert_eq!(
        s.client.try_rotate_member(&s.c1, &s.ana, &x),
        Err(Ok(Error::NotCommitteeMember))
    );
    assert_eq!(
        s.client.try_rotate_member(&s.c1, &s.c2, &s.c3),
        Err(Ok(Error::InvalidCommittee))
    );
    assert_eq!(
        s.client.try_rotate_member(&s.ana, &s.c2, &x),
        Err(Ok(Error::NotCommitteeMember))
    );
}

#[test]
fn revocar_miembro_con_mayoria_sin_romper_el_umbral() {
    let s = setup();
    assert!(!s.client.revoke_member(&s.c1, &s.c3));
    assert!(s.client.revoke_member(&s.c2, &s.c3));
    assert_eq!(s.client.get_config().committee.len(), 2);
    assert!(!s.client.is_member(&s.c3));

    assert_eq!(
        s.client.try_revoke_member(&s.c1, &s.c2),
        Err(Ok(Error::InvalidCommittee))
    );
}
