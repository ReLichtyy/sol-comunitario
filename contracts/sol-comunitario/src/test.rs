#![cfg(test)]
extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    vec, Address, Env, String,
};

struct Setup<'a> {
    env: Env,
    client: SolComunitarioClient<'a>,
    token: TokenClient<'a>,
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

fn new_project_with(s: &Setup, name: &str, kwh: i128, advance_bps: u32) -> u32 {
    s.client.create_project(
        &s.installer,
        &s.c1,
        &String::from_str(&s.env, name),
        &kwh,
        &advance_bps,
        &30,
        &90,
    )
}

/// Proyecto típico: 1 000 kWh en 90 días, 30% de adelanto, 30 días para instalar.
fn new_project(s: &Setup) -> u32 {
    new_project_with(s, "Salon comunal de Santa Teresa", 1_000, 3_000)
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
    s.client.contribute(&s.ana, &id, &7_500);
    s.client.contribute(&s.beto, &id, &2_500);
    assert_eq!(s.client.get_project(&id).total, 10_000);

    advance_days(&s.env, 10);
    // Medidor nuevo marca 5 000. Adelanto del 30% al confirmar.
    install(&s, id, 5_000);
    assert_eq!(s.token.balance(&s.installer), 3_000);

    // Lecturas mensuales ABSOLUTAS del medidor: termina en 5 800 => 800 kWh.
    for meter in [5_250, 5_550, 5_800] {
        advance_days(&s.env, 30);
        read(&s, id, meter);
    }
    assert_eq!(s.client.get_project(&id).kwh_generated, 800);
    assert_eq!(s.client.performance_bps(&id), 8_000);

    advance_days(&s.env, 1 + OBJECTION_DAYS);
    // Ganó 80% = 8 000; ya recibió 3 000 => paga 5 000.
    assert_eq!(s.client.settle(&id), 5_000);
    assert_eq!(s.token.balance(&s.installer), 8_000);

    // Sobran 2 000 => 75% a Ana y 25% a Beto.
    assert_eq!(s.client.claim_refund(&s.ana, &id), 1_500);
    assert_eq!(s.client.claim_refund(&s.beto, &id), 500);
    assert_eq!(s.token.balance(&s.client.address), 0);
}

// ---------------- comité 2 de 3 ----------------

#[test]
fn una_sola_firma_no_libera_el_adelanto() {
    let s = setup();
    let id = new_project(&s);
    s.client.contribute(&s.ana, &id, &10_000);

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
    s.client.contribute(&s.ana, &id, &1_000);
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
    s.client.contribute(&s.ana, &id, &1_000);
    assert_eq!(
        s.client.try_approve_installation(&s.installer, &id, &0),
        Err(Ok(Error::NotCommitteeMember))
    );
    assert_eq!(
        s.client.try_create_project(
            &s.installer,
            &s.ana,
            &String::from_str(&s.env, "x"),
            &1_000,
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
    s.client.contribute(&s.ana, &id, &1_000);
    install(&s, id, 0);

    // c1 dice 300, c2 dice 250: ninguna lectura tiene mayoría.
    assert!(!s.client.submit_reading(&s.c1, &id, &300));
    assert!(!s.client.submit_reading(&s.c2, &id, &250));
    assert_eq!(s.client.get_project(&id).kwh_generated, 0);

    // c3 confirma 250 => se aplica 250.
    assert!(s.client.submit_reading(&s.c3, &id, &250));
    assert_eq!(s.client.get_project(&id).kwh_generated, 250);
}

#[test]
fn el_medidor_nunca_retrocede() {
    let s = setup();
    let id = new_project(&s);
    s.client.contribute(&s.ana, &id, &1_000);
    install(&s, id, 5_000);
    read(&s, id, 5_400);

    // Reportar el mismo mes dos veces o un valor menor es rechazado.
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
    s.client.contribute(&s.ana, &id, &10_000);
    install(&s, id, 0);
    read(&s, id, 1_500);
    advance_days(&s.env, 91 + OBJECTION_DAYS);
    assert_eq!(s.client.settle(&id), 7_000);
    assert_eq!(s.token.balance(&s.installer), 10_000);
    assert_eq!(s.client.performance_bps(&id), 10_000);
    assert_eq!(s.client.claim_refund(&s.ana, &id), 0);
}

#[test]
fn paneles_que_no_generan_instalador_solo_se_queda_el_adelanto() {
    let s = setup();
    let id = new_project(&s);
    s.client.contribute(&s.ana, &id, &10_000);
    install(&s, id, 0);
    read(&s, id, 100); // 10% < 30% de adelanto
    advance_days(&s.env, 91 + OBJECTION_DAYS);
    assert_eq!(s.client.settle(&id), 0);
    assert_eq!(s.token.balance(&s.installer), 3_000);
    assert_eq!(s.client.claim_refund(&s.ana, &id), 7_000);
}

#[test]
fn nunca_se_instala_vecinos_recuperan_todo() {
    let s = setup();
    let id = new_project(&s);
    s.client.contribute(&s.ana, &id, &4_000);

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
        s.client.try_contribute(&s.beto, &id, &100),
        Err(Ok(Error::InstallDeadlinePassed))
    );
    assert_eq!(s.client.claim_refund(&s.ana, &id), 4_000);
    assert_eq!(s.token.balance(&s.ana), 100_000);
}

// ---------------- tiempos y estados ----------------

#[test]
fn no_se_liquida_antes_de_tiempo_ni_se_mide_despues() {
    let s = setup();
    let id = new_project(&s);
    s.client.contribute(&s.ana, &id, &1_000);
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
    s.client.contribute(&s.ana, &id, &1_000);
    install(&s, id, 0);
    read(&s, id, 500);
    advance_days(&s.env, 91 + OBJECTION_DAYS);
    s.client.settle(&id);

    assert_eq!(s.client.try_settle(&id), Err(Ok(Error::NotMeasuring)));
    assert_eq!(s.client.claim_refund(&s.ana, &id), 500);
    assert_eq!(
        s.client.try_claim_refund(&s.ana, &id),
        Err(Ok(Error::NothingToClaim))
    );
}

#[test]
fn no_se_instala_sin_fondos_ni_se_aporta_despues_de_instalar() {
    let s = setup();
    let id = new_project(&s);
    assert_eq!(
        s.client.try_approve_installation(&s.c1, &id, &0),
        Err(Ok(Error::NoFunds))
    );
    s.client.contribute(&s.ana, &id, &1_000);
    install(&s, id, 0);
    assert_eq!(
        s.client.try_contribute(&s.beto, &id, &1_000),
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
fn validaciones_de_creacion_y_montos() {
    let s = setup();
    let name = String::from_str(&s.env, "Escuela de Nosara");
    let mk = |kwh: i128, bps: u32, inst: u32| {
        s.client
            .try_create_project(&s.installer, &s.c1, &name, &kwh, &bps, &inst, &90)
    };
    assert_eq!(mk(0, 3_000, 30), Err(Ok(Error::InvalidPromise)));
    assert_eq!(mk(1_000, 6_000, 30), Err(Ok(Error::InvalidAdvance)));
    assert_eq!(mk(1_000, 3_000, 0), Err(Ok(Error::InvalidDays)));

    let long = String::from_str(&s.env, &"x".repeat(MAX_NAME_LEN as usize + 1));
    assert_eq!(
        s.client
            .try_create_project(&s.installer, &s.c1, &long, &1_000, &0, &30, &90),
        Err(Ok(Error::NameTooLong))
    );

    let id = new_project_with(&s, "Escuela de Nosara", 1_000, 0);
    assert_eq!(
        s.client.try_contribute(&s.ana, &id, &0),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        s.client.try_contribute(&s.ana, &42, &10),
        Err(Ok(Error::ProjectNotFound))
    );
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
    // umbral 0, umbral > miembros, miembros repetidos
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
fn crear_proyecto_requiere_firma_de_instalador_y_miembro() {
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
    let id = new_project_with(&s, "EBAIS de Pital", 7, 3_333);
    s.client.contribute(&s.ana, &id, &1_001);
    s.client.contribute(&s.beto, &id, &333);
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
    s.client.contribute(&s.ana, &id, &10_000);
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
fn vecino_objeta_y_el_comite_corrige_la_lectura() {
    let s = setup();
    let id = inflated_project(&s);
    advance_days(&s.env, 91);

    s.client.dispute(&s.ana, &id);
    assert_eq!(s.client.get_project(&id).status, Status::Disputed);

    // Aunque pase el plazo, un proyecto en disputa no se liquida solo.
    advance_days(&s.env, 30);
    assert_eq!(s.client.try_settle(&id), Err(Ok(Error::InDispute)));

    // Mayoría del comité fija la lectura real (600) y se liquida en el acto.
    assert!(!s.client.resolve_dispute(&s.c1, &id, &600));
    assert!(s.client.resolve_dispute(&s.c3, &id, &600));

    let p = s.client.get_project(&id);
    assert_eq!(p.status, Status::Settled);
    assert_eq!(p.kwh_generated, 600);
    // 60% de 10 000 = 6 000 (3 000 adelanto + 3 000 final)
    assert_eq!(s.token.balance(&s.installer), 6_000);
    assert_eq!(s.client.claim_refund(&s.ana, &id), 4_000);
}

#[test]
fn solo_quien_aporto_puede_objetar() {
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
    // Todavía midiendo.
    assert_eq!(
        s.client.try_dispute(&s.ana, &id),
        Err(Ok(Error::NotDisputable))
    );
    // Ya pasó la ventana de objeción.
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

    // Luis (c2) ya no tiene su llave: firman Marta (c1) y Sofía (c3).
    assert!(!s.client.rotate_member(&s.c1, &s.c2, &luis_nuevo));
    assert!(s.client.rotate_member(&s.c3, &s.c2, &luis_nuevo));

    // Mismo rol, nueva llave, nueva fecha de vigencia.
    let cfg = s.client.get_config();
    let luis = cfg.committee.iter().find(|m| m.account == luis_nuevo).unwrap();
    assert_eq!(luis.role, String::from_str(&s.env, "Fontanero ASADA"));
    assert_eq!(luis.since, 5 * DAY);
    assert!(!s.client.is_member(&s.c2));

    // La llave vieja ya no firma; la nueva sí.
    let id = new_project(&s);
    s.client.contribute(&s.ana, &id, &1_000);
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
    // `old` debe ser miembro.
    assert_eq!(
        s.client.try_rotate_member(&s.c1, &s.ana, &x),
        Err(Ok(Error::NotCommitteeMember))
    );
    // `new` no puede ser alguien que ya está en el comité.
    assert_eq!(
        s.client.try_rotate_member(&s.c1, &s.c2, &s.c3),
        Err(Ok(Error::InvalidCommittee))
    );
    // Un extraño no puede firmar la rotación.
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

    // Quedan 2 con umbral 2: revocar otro dejaría el comité inservible.
    assert_eq!(
        s.client.try_revoke_member(&s.c1, &s.c2),
        Err(Ok(Error::InvalidCommittee))
    );
}
