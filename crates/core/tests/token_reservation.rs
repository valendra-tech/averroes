use averroes_core::runtime::{ResourceGovernor, TokenReservation};

#[test]
fn public_reservation_can_reconcile_usage() {
    let governor = ResourceGovernor::new(1, 10);
    let mut reservation: TokenReservation = governor.reserve_tokens(4).unwrap();

    assert!(reservation.reconcile(2));
    assert_eq!(governor.tokens_available(), 8);
}
