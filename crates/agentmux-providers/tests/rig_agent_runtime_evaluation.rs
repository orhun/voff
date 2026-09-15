//! Phase 4 characterization of Rig's optional classic agent runtime.
//!
//! AgentMux deliberately does not use this type as persisted production state.
//! These tests pin the evaluated API and the ownership boundary behind the
//! provider-adapter crate.

use rig_agent::AgentRun;
use rig_agent::agent::run::AgentRunStep;

#[test]
fn sans_io_run_can_round_trip_before_provider_io() {
    let run = AgentRun::new("inspect the workspace").max_turns(2);
    let encoded = serde_json::to_vec(&run).expect("Rig run should serialize");
    let mut restored: AgentRun =
        serde_json::from_slice(&encoded).expect("Rig run should deserialize");

    match restored.next_step().expect("restored run should advance") {
        AgentRunStep::CallModel {
            prompt,
            history,
            turn,
        } => {
            assert_eq!(
                prompt,
                rig_agent::core::message::Message::user("inspect the workspace")
            );
            assert!(history.is_empty());
            assert_eq!(turn, 1);
        }
        _ => panic!("fresh run must request its first model call"),
    }
}

#[test]
fn suspended_run_preserves_its_protocol_position() {
    let mut run = AgentRun::new("inspect the workspace").max_turns(2);
    assert!(matches!(
        run.next_step().expect("first step should be available"),
        AgentRunStep::CallModel { turn: 1, .. }
    ));

    let encoded = serde_json::to_vec(&run).expect("suspended run should serialize");
    let mut restored: AgentRun =
        serde_json::from_slice(&encoded).expect("suspended run should deserialize");

    assert_eq!(restored.turn(), 1);
    assert!(
        restored.next_step().is_err(),
        "a resumed driver must supply the pending model response instead of issuing a duplicate request"
    );
}

#[test]
fn production_manifests_do_not_adopt_rig_agent_state() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("providers crate should be below the workspace root");

    for crate_name in [
        "agentmux-core",
        "agentmux-provider",
        "agentmux-engine",
        "agentmux-storage",
        "agentmux-tools",
        "agentmux-executor",
        "agentmux-tui",
        "agentmux-cli",
    ] {
        let manifest =
            std::fs::read_to_string(workspace.join("crates").join(crate_name).join("Cargo.toml"))
                .expect("manifest should be readable");
        assert!(
            !manifest.contains("rig-agent"),
            "Rig agent runtime crossed into {crate_name}"
        );
    }
}

#[test]
fn optional_rig_execution_and_effect_stacks_stay_out_of_production() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("providers crate should be below the workspace root");
    let provider_manifest =
        std::fs::read_to_string(workspace.join("crates/agentmux-providers/Cargo.toml"))
            .expect("provider manifest should be readable");
    assert!(provider_manifest.contains("rig-agent={version=\"=0.42.0\",default-features=false}"));
    assert!(!provider_manifest.contains("features=[\"rmcp\"]"));

    let lockfile =
        std::fs::read_to_string(workspace.join("Cargo.lock")).expect("lockfile should be readable");
    assert!(!lockfile.contains("name = \"rig-effect-log\""));
    assert!(!lockfile.contains("name = \"rmcp\""));
}
