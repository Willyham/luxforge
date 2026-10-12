//! The overview's ledger and want, without a device.
use super::*;

#[test]
fn the_overview_ledger_is_capped_within_the_preview_budget() {
    let figures = super::super::Figures::default();
    figures
        .budget
        .store(64 << 20, std::sync::atomic::Ordering::Release);
    figures.charge_overview(3 << 20).unwrap();
    assert_eq!(figures.overview(), 3 << 20);
    assert_eq!(figures.in_use(), 3 << 20);
    // Past the overview's 4 MiB: refused, neither ledger charged.
    let refused = figures.charge_overview(2 << 20).unwrap_err();
    assert!(matches!(
        refused,
        GpuFallback::BudgetExceeded { budget, .. } if budget == OVERVIEW_GPU_BUDGET
    ));
    assert_eq!(figures.overview(), 3 << 20);
    assert_eq!(figures.in_use(), 3 << 20);
    figures.charge_overview(1 << 20).unwrap();
    assert_eq!(figures.overview(), OVERVIEW_GPU_BUDGET);
}

#[test]
fn an_overview_the_preview_budget_cannot_hold_is_refused_and_uncharged() {
    let figures = super::super::Figures::default();
    figures
        .budget
        .store(1 << 20, std::sync::atomic::Ordering::Release);
    figures.charge(1 << 19).unwrap();
    assert!(figures.charge_overview(1 << 20).is_err());
    assert_eq!(figures.overview(), 0);
    assert_eq!(figures.in_use(), 1 << 19);
}

#[test]
fn a_want_names_the_settled_and_the_awaited_overview() {
    let want = OverviewWant {
        keep: Some(3),
        awaited: Some(5),
        upload: None,
    };
    assert!(want.wants(3) && want.wants(5));
    assert!(!want.wants(4));
    assert!(!OverviewWant::default().wants(0));
    assert_eq!(
        texture_bytes((256, 171)),
        256 * 171 * 4 + UNIFORM_SIZE as u64
    );
    // Current and replacement at the largest size fit the ledger beside their reduction's parts.
    let largest = texture_bytes((256, 256));
    assert!(2 * largest + 256 * 256 * 16 < OVERVIEW_GPU_BUDGET);
}
