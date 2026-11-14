# A role ceiling cannot grant: service roles need a predicate of their own

**Symptom.** The obvious way to give an out-of-cluster runner a narrow
credential (#828) was `ESCUREL_AUTH_ROLE_TOOLS=svc:runner=claim_event,...`.
It cannot work, for two reasons that are easy to miss:

1. `ESCUREL_AUTH_ROLE_TOOLS` is a **ceiling**: a token whose groups carry a
   listed role may call *only* those tools, intersected with everything else.
   `claim_event`, `renew_claim` and `mint_run_token` are admin-scope, so a
   non-admin token is refused them at dispatch (`admin_required`) before the
   ceiling is consulted. A ceiling can take tools away; it never adds one.
2. Most of what a runner does is not a tool at all but a **bookkeeper check
   inside a tool**: `capture_event` refusing `run:` / `sched:` / `lint-` ids,
   `kind: system` and `escurel:` labels; `report_runner_status`;
   `put_run_transcript` for a run it holds no token for; the admin-only
   `runtime` page write; reading every inbox event so it can claim. Those were
   written as `caller.is_admin && !is_machine_caller(caller)` in seven places.
   A tool list does not reach any of them. (`RoleTools::parse` also refuses
   `escurel:`-prefixed roles, so the role could not even be named that way.)

**Fix.** Built-in service roles in the verifier (`AuthContext.service`,
`ServiceRole::{Runner, Bridge}`, configured like the admin value and ignored
on any machine token), one predicate for the bookkeeper checks
(`AclCaller::may_keep_books`: admin or runner, never a machine caller), and a
named grant list for the admin-scope tools a runner calls
(`schema::RUNNER_ADMIN_TOOLS`), consulted by the dispatch admin gate and by
`tools/list`. The ceiling stays what it is: a restriction for app service
roles.

**Recognise it next time.** "Let role X do Y" where Y is an admin tool or an
in-handler `is_admin` check: a ceiling is the wrong tool. Look for the
predicate the checks share and widen *that*, in one place, with a test that a
machine token carrying the role gets none of it
(`event_claims::a_runner_role_keeps_its_books_and_nothing_more`).
