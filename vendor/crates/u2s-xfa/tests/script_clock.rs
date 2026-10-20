//! `SOURCE_DATE_EPOCH` fixes the clock form scripts read. A test binary of
//! its own, because the variable is process-wide.

use u2s_xfa::xfa::scripting::XfaScriptEngine;

#[test]
fn source_date_epoch_is_the_time_scripts_see() {
    // SAFETY: the only test in this binary, so no other thread reads the
    // environment concurrently.
    unsafe { std::env::set_var("SOURCE_DATE_EPOCH", "1768478400") };
    let mut engine = XfaScriptEngine::new();
    let now = engine
        .evaluate_expression("new Date().toISOString()")
        .expect("evaluate");
    assert_eq!(now, "2026-01-15T12:00:00.000Z");
    let local = engine
        .evaluate_expression("var d = new Date(); d.getDate() + '.' + (d.getMonth() + 1) + ' ' + d.getHours()")
        .expect("evaluate");
    assert_eq!(local, "15.1 12", "the fixed clock reads in UTC");
}
