#![cfg(test)]

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
        &vec![&env, c1.clone(), c2.clone(), c3.clone()],
        &2,
        &sac.address(),
    );

    let token = TokenClient::new(&env, &sac.address());
    Setup { env, client, token, installer, c1, c2, c3, ana, beto }
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

    advance_days(&s.env, 1);
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
    advance_days(&s.env, 91);
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
    advance_days(&s.env, 91);
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
    advance_days(&s.env, 91);
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
            .try_initialize(&vec![&s.env, other.clone()], &1, &other),
        Err(Ok(Error::AlreadyInitialized))
    );

    let env = Env::default();
    env.mock_all_auths();
    let fresh = SolComunitarioClient::new(&env, &env.register(SolComunitario, ()));
    let a = Address::generate(&env);
    let b = Address::generate(&env);
    // umbral 0, umbral > miembros, miembros repetidos
    assert_eq!(
        fresh.try_initialize(&vec![&env, a.clone(), b.clone()], &0, &a),
        Err(Ok(Error::InvalidCommittee))
    );
    assert_eq!(
        fresh.try_initialize(&vec![&env, a.clone(), b.clone()], &3, &a),
        Err(Ok(Error::InvalidCommittee))
    );
    assert_eq!(
        fresh.try_initialize(&vec![&env, a.clone(), a.clone()], &2, &a),
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
    advance_days(&s.env, 91);
    s.client.settle(&id);
    s.client.claim_refund(&s.ana, &id);
    s.client.claim_refund(&s.beto, &id);
    let left = s.token.balance(&s.client.address);
    assert!((0..=2).contains(&left));
}
