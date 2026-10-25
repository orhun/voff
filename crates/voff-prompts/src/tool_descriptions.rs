//! Built-in descriptions supplied to model tool calls.

pub const FILE_READ: &str = "Inspect a bounded page of a workspace text file before editing. offset is a character offset (start at 0); each page has at most 6000 characters. Batch independent reads in one response and reuse the recorded artifact. If a file is missing, create it with an approved file.patch instead of rereading it. Reads are workspace-scoped and do not require approval; sensitive paths are denied. Results are persisted as file_read artifacts.";
pub const SEARCH_TEXT: &str = "Search workspace text for an exact string (query up to 256 bytes). Limit results to 100 lines. Search is workspace-scoped, bounded, read-only and does not require approval; sensitive paths and files over 256 KiB are excluded. Results are persisted as search_result artifacts.";
pub const FILE_PATCH: &str = "Propose one coherent patch set for the configured workspace: use path/replacement for one file, or patches with 2–12 distinct path/replacement entries when several files belong to the same change. Each file is checked against its current base, and one approval reviews the exact full set. Keep the set focused; a previously rejected action still requires a fresh review.";
pub const PROCESS_TEST: &str = "Propose one bounded, task-scoped local process using its exact executable and argv array. State its purpose and whether it verifies acceptance. The Team Lead/root reviews every invocation and its likely side effects before execution. Runs directly without a shell, from the workspace root, with a 5-minute timeout and capped output. Run task-owned test scripts directly; never pass source code through inline execution flags such as node -e or python -c. Do not pass secrets or request network, installation, publication, deployment, destructive, or unrelated actions.";
pub const DELEGATE: &str = "Delegate when useful to an enabled direct report. For multiple coordinated implementations, define the shared interface, disjoint workspace-relative owned_paths with one writer per path, and integration work that checks the combined result. If acceptance requires behavioral tests and no suitable harness exists, include a small test file in an owner's paths. A single bounded task may be assigned directly. Never invent capabilities.";
pub const COMPLETE: &str = "Request final acceptance with one evidence entry for every acceptance criterion. Run the required verification and cite persisted artifact IDs. If Voff returns workflow feedback, continue working and resubmit only after addressing it. A patch alone cannot complete work.";
pub const BLOCKED: &str = "Report a concrete blocker when required work or verification cannot be completed within the current scope. This does not complete the task or widen permissions.";
pub const REVIEW: &str = "Request review by delegating a bounded evidence review task.";
pub const WORKFLOW_DECIDE: &str =
    "Return a review verdict; no side effects are performed by this tool.";
pub const JSON_TOOL_ARGUMENTS: &str = "Tool arguments encoded as one JSON object string";

pub const DELEGATION_PLAN_TEMPLATE: &str = "Propose the complete bounded child-task plan in one call. Put one independently approvable workstream in each proposals item and assign it to the best-qualified enabled target. Independent items can run concurrently. Exact target_agent values: {targets}.";
pub const DELEGATION_TEMPLATE: &str =
    "{description} Use an exact target_agent value from: {targets}.";

fn interpolate(template: &str, values: &[(&str, &str)]) -> String {
    let mut output = String::with_capacity(template.len());
    let mut remaining = template;
    while let Some((index, key, value)) = values
        .iter()
        .filter_map(|(key, value)| remaining.find(key).map(|index| (index, *key, *value)))
        .min_by_key(|(index, _, _)| *index)
    {
        output.push_str(&remaining[..index]);
        output.push_str(value);
        remaining = &remaining[index + key.len()..];
    }
    output.push_str(remaining);
    output
}

pub fn delegation_plan(targets: &str) -> String {
    interpolate(DELEGATION_PLAN_TEMPLATE, &[("{targets}", targets)])
}

pub fn delegation(description: &str, targets: &str) -> String {
    interpolate(
        DELEGATION_TEMPLATE,
        &[("{description}", description), ("{targets}", targets)],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_tool_descriptions_preserve_template_markers() {
        assert!(delegation("Review {targets}", "Backend").contains("Review {targets}"));
        assert!(delegation_plan("{targets}").ends_with("Exact target_agent values: {targets}."));
    }
}
