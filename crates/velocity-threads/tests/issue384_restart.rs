// Risk (#384): run PR duty's unchanged unread-work reproduction with workspace tests,
// even when a cached standalone stress runner still embeds a deleted warm-pool path.
include!("../../../stress/issue384-restart-empty-dispatch.rs");

#[test]
fn interrupted_queued_work_remains_resumable() -> Result<(), Box<dyn std::error::Error>> {
    main()
}
