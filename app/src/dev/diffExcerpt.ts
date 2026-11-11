/**
 * A real core-diff excerpt for the dev fixtures: two hunks of commit 233cf80
 * in this repository, cut the way `agendar deliver` cuts them. Development
 * only; `vite build` strips it.
 */

export const RECEIPT_FREE_EXCERPT = "diff --git a/crates/agendar-core/src/model/validate.rs b/crates/agendar-core/src/model/validate.rs\n" +
"index 96fdba3..ce8a1ad 100644\n" +
"--- a/crates/agendar-core/src/model/validate.rs\n" +
"+++ b/crates/agendar-core/src/model/validate.rs\n" +
"@@ -1265,8 +1266,17 @@ fn validate_decision_ack(body: &DecisionAck) -> Result<()> {\n" +
"     Ok(())\n" +
" }\n" +
" \n" +
"-fn validate_delivery(body: &Delivery) -> Result<()> {\n" +
"+fn validate_delivery(body: &Delivery, event: &Event) -> Result<()> {\n" +
"     validate_text(\"/name\", &body.name, 1, 80, false)?;\n" +
"+    // Criteria are a receipt's done_when; without a receipt the disclosure\n" +
"+    // carries the claims.\n" +
"+    if event.refs.receipt.is_none() && !body.criteria.is_empty() {\n" +
"+        return Err(validation_at(\n" +
"+            \"/criteria\",\n" +
"+            \"delivery_criteria_without_receipt\",\n" +
"+            \"A delivery without a receipt must list no criteria.\",\n" +
"+        ));\n" +
"+    }\n" +
"     if body.demos.is_empty() || body.demos.len() > 3 {\n" +
"         return Err(validation_at(\n" +
"             \"/demos\",\n" +
"diff --git a/crates/agendar-core/src/reduce/evidence.rs b/crates/agendar-core/src/reduce/evidence.rs\n" +
"index 1d20b79..bbd67c3 100644\n" +
"--- a/crates/agendar-core/src/reduce/evidence.rs\n" +
"+++ b/crates/agendar-core/src/reduce/evidence.rs\n" +
"@@ -146,18 +146,24 @@ pub fn finalize(\n" +
"         let EventPayload::Delivery(body) = &event.payload else {\n" +
"             continue;\n" +
"         };\n" +
"-        let Some(receipt_id) = event.refs.receipt.clone() else {\n" +
"-            continue;\n" +
"-        };\n" +
"-        let Some(receipt) = receipts.get(&receipt_id) else {\n" +
"-            // The resolver keeps a delivery pending until its receipt exists.\n" +
"-            continue;\n" +
"+        // A delivery of finished work no decision asked for has no receipt.\n" +
"+        let receipt_id = event.refs.receipt.clone();\n" +
"+        let receipt = match &receipt_id {\n" +
"+            Some(id) => match receipts.get(id) {\n" +
"+                Some(receipt) => Some(receipt),\n" +
"+                // The resolver keeps a delivery pending until its receipt exists.\n" +
"+                None => continue,\n" +
"+            },\n" +
"+            None => None,\n" +
"         };\n" +
"-        let node = event\n" +
"+        let Some(node) = event\n" +
"             .refs\n" +
"             .node\n" +
"             .clone()\n" +
"-            .unwrap_or_else(|| receipt.node.clone());\n" +
"+            .or_else(|| receipt.map(|receipt| receipt.node.clone()))\n" +
"+        else {\n" +
"+            continue;\n" +
"+        };\n" +
"         let superseded_by = superseded.get(delivery_id).cloned();\n" +
"         let criteria = project_criteria(fold, delivery_id, body, receipt, causal, order_index);\n" +
"         let observation_frontier: Vec<EventId> = criteria\n";

export const RECEIPT_FREE_DIFF = {
  "commit": "233cf80bebc689ffa7ca698deff2d95d16437dad",
  "files_total": 18,
  "hunks_total": 53,
  "added_total": 543,
  "removed_total": 144,
  "shown": [
    {
      "path": "crates/agendar-core/src/model/validate.rs",
      "hunk": 4,
      "why": "A delivery without a receipt must list no criteria."
    },
    {
      "path": "crates/agendar-core/src/reduce/evidence.rs",
      "hunk": 2,
      "why": "The projection keeps a receipt-free delivery instead of skipping it."
    }
  ],
  "omitted": [
    {
      "path": "app/src-tauri/src/host.rs",
      "added": 7,
      "removed": 3
    },
    {
      "path": "app/src-tauri/src/projection.rs",
      "added": 5,
      "removed": 4
    },
    {
      "path": "app/src-tauri/tests/backend.rs",
      "added": 76,
      "removed": 4
    },
    {
      "path": "app/src/api/types.ts",
      "added": 2,
      "removed": 1
    },
    {
      "path": "app/src/test/delivery.test.tsx",
      "added": 16,
      "removed": 0
    },
    {
      "path": "app/src/views/Delivery.tsx",
      "added": 42,
      "removed": 38
    },
    {
      "path": "crates/agendar-cli/src/args.rs",
      "added": 3,
      "removed": 1
    },
    {
      "path": "crates/agendar-cli/src/commands.rs",
      "added": 5,
      "removed": 5
    },
    {
      "path": "crates/agendar-cli/src/delivery.rs",
      "added": 5,
      "removed": 2
    },
    {
      "path": "crates/agendar-cli/tests/delivery.rs",
      "added": 74,
      "removed": 0
    },
    {
      "path": "crates/agendar-core/src/model/state.rs",
      "added": 4,
      "removed": 1
    },
    {
      "path": "crates/agendar-core/src/reduce/budget.rs",
      "added": 1,
      "removed": 1
    },
    {
      "path": "crates/agendar-core/src/service.rs",
      "added": 48,
      "removed": 30
    },
    {
      "path": "crates/agendar-core/tests/checks.rs",
      "added": 166,
      "removed": 1
    },
    {
      "path": "crates/agendar-core/tests/wire.rs",
      "added": 13,
      "removed": 1
    },
    {
      "path": "skills/agendar/SKILL.md",
      "added": 29,
      "removed": 27
    }
  ]
};
