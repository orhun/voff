use std::time::Instant;
use voff_core::{CanonicalEvent, CanonicalEventKind, EventId, FormationId, Session, SessionId};
use voff_storage::{Repository, SqliteRepository};

fn main() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        let repository = SqliteRepository::open_in_memory()
            .await
            .expect("repository");
        let session = Session {
            id: SessionId::new(),
            formation_id: FormationId::new(),
            workspace: ".".into(),
            created_at: chrono::Utc::now(),
        };
        repository
            .save_session(session.clone())
            .await
            .expect("session");

        let started = Instant::now();
        for index in 0..10_000 {
            repository
                .append_event(CanonicalEvent {
                    id: EventId::new(),
                    session_id: session.id,
                    task_id: None,
                    created_at: chrono::Utc::now(),
                    kind: CanonicalEventKind::TextDelta(format!("event {index}")),
                })
                .await
                .expect("event");
        }
        let write_elapsed = started.elapsed();
        let started = Instant::now();
        let events = repository.events(session.id).await.expect("events");
        let read_elapsed = started.elapsed();
        assert_eq!(events.len(), 10_000);
        println!(
            "large_history events=10000 write_ms={} read_ms={}",
            write_elapsed.as_millis(),
            read_elapsed.as_millis()
        );
    });
}
