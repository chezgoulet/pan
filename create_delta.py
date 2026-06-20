#!/usr/bin/env python3
import subprocess, json, time

# Read token without any interpolation issues
tok_file = open('/opt/data/.github_token')
tok = tok_file.read().strip()
tok_file.close()

base = 'https://api.github.com/repos/chezgoulet/pan'
auth_hdr = 'Authorization: Bearer ' + tok

def create_issue(title, body, labels):
    data = json.dumps({'title': title, 'body': body, 'labels': labels})
    req = subprocess.run(
        ['curl', '-s', '-X', 'POST',
         '-H', auth_hdr,
         '-H', 'Content-Type: application/json',
         '-H', 'Accept: application/vnd.github.v3+json',
         base + '/issues', '-d', data],
        capture_output=True, text=True
    )
    result = json.loads(req.stdout)
    n = result.get('number', 'ERR')
    msg = result.get('message', '')
    if msg and msg != 'Created':
        print('  ERROR #' + str(n) + ': ' + msg)
    else:
        print('  #' + str(n) + ': ' + title)
    time.sleep(0.35)
    return n

def add_comment(issue_num, body):
    data = json.dumps({'body': body})
    req = subprocess.run(
        ['curl', '-s', '-X', 'POST',
         '-H', auth_hdr,
         '-H', 'Content-Type: application/json',
         '-H', 'Accept: application/vnd.github.v3+json',
         base + '/issues/' + str(issue_num) + '/comments', '-d', data],
        capture_output=True, text=True
    )
    result = json.loads(req.stdout)
    n = result.get('id', 'ERR')
    return n

print('=== NEW ISSUES ===')

create_issue(
    'Persona: identity and span binding',
    '## Description\nIntroduce Persona as a first-class identity concept and bind it to the span. A Persona is a persistent, named identity within one Pan process — it owns its own soul/state, memory scope, governance context, and budget envelope. This is the Pan equivalent of a Hermes "profile" or an OpenClaw "agent."\n\n## Design rule\nPersona is a *scope*, not a config object. It is not a field that plugins read and filter on. It is the scope to which a span\'s handles are bound. Isolation is by construction (the capability-handle principle, now carrying identity), never by plugin discipline.\n\n## Where Persona lives in the type model\nPersona does NOT hang off Goal. A Persona spans many goals. Introduce a thin `SpanContext { persona: PersonaId, goal: Goal }` wrapper that the loop operates on. Goal.revision stays where it is. The leak-test crate (lib.rs) models Goal directly with no span wrapper — this issue introduces SpanContext in pan-core.\n\n## Acceptance Criteria\n- [ ] Persona identity type exists (opaque: PersonaId newtype over String)\n- [ ] SpanContext type exists: { persona: PersonaId, goal: Goal }\n- [ ] The loop operates on SpanContext, threading persona to every phase (observe/decide/enact/commit)\n- [ ] Persona is immutable for the life of the span\n- [ ] A span cannot be opened without a Persona (single-identity deployments pass a default Persona explicitly; there is no "no identity" path)\n- [ ] No plugin can mutate the Persona mid-span\n- [ ] Unit tests: Persona is carried through all four phases unchanged\n- [ ] The Vocab types in pan-core are updated to include SpanContext (no change to pan-schema crate\'s leak-test types yet)\n\n## Dependencies\n- #2 (vocabulary types — SpanContext wraps Goal)\n\n## Risk\nLow if done now. High if deferred — every later handle and plugin would assume single-identity and require rework.',
    ['wave-0-core', 'core', 'sprint-1']
)

create_issue(
    'Per-Persona memory write concurrency',
    '## Description\nThe report Section 14.2 concurrency seam is now activated: multiple Personas writing to shared memory (Ragamuffin) concurrently is steady state in a multi-identity deployment. Decide and implement the write-concurrency policy for persona-scoped memory stores.\n\n## Constraints\n- Memory writes go through the dispatch pipeline as a governed Invoke of cap.state_write (already true) — so a serialization point exists\n- Two Personas writing different identities must not block each other\n- Two writes to the *same* Persona must not produce a lost update\n\n## Recommended approach\nPer-Persona single-writer: a persona-scoped actor/queue serializing that Persona\'s writes. This matches the one-identity-serialized-writes model and avoids cross-Persona contention entirely. Alternative: optimistic versioning with conflict rejection, if Ragamuffin\'s consistency guarantees favor it.\n\n## Acceptance Criteria\n- [ ] Write-concurrency policy is chosen and documented in the memory plugin\'s architecture notes\n- [ ] Concurrent writes to two different Personas do not block each other\n- [ ] Concurrent writes to the *same* Persona are serialized (no lost update)\n- [ ] Test: two threads write to Persona A and Persona B simultaneously — both succeed, A\'s writes are in order, B\'s writes are in order\n- [ ] Test: two threads write to Persona A simultaneously — writes are serialized, no data corruption\n\n## Dependencies\n- #25 (memory.vector — the Ragamuffin client this concurrency policy governs)\n- #47 (Persona — concurrency is per-Persona)\n\n## Risk\nMedium — getting this wrong corrupts memory under concurrency. Deferring past Wave 3 means shipping multi-identity without a concurrency story.',
    ['wave-3-memory', 'plugin', 'sprint-3']
)

create_issue(
    'channel.paperclip adapter',
    '## Description\nAn adapter that lets Paperclip hire the Pan gateway and drive its Personas. Paperclip is the orchestration control plane — it manages org charts, budgets, governance, and multi-company isolation. Pan is one gateway that Paperclip hires; this channel adapter is the integration surface.\n\n## What the adapter does\n- Receives a Paperclip heartbeat and turns it into an observation/goal (reuses the sched/admission/observe machinery from #44)\n- Resolves Paperclip\'s injected agent identity to a Pan Persona (creating or loading the Persona\'s soul/state)\n- Honors injected goal context, budget envelope, and secrets for the span\n- Reports structured logs, cost events, session state, and audit trail back to Paperclip, sourced from Pan\'s event stream\n- Persists session state across heartbeats per Persona (Paperclip expects agents to resume, not restart)\n\n## Integration surface\n\nPaperclip provides: Agent identity, Goal context/task, Budget envelope, Secrets/credentials, Expected result format\nPan consumes via: Resolved to Persona at span-open, Fills Goal.objective and trigger, Feeds gov.policy hard-stop, Feeds gov.secrets, Channel delivers back structured results\n\n## Acceptance Criteria\n- [ ] Registers as channel.paperclip\n- [ ] Receives heartbeat -> Goal::Event observation via the observe pipeline\n- [ ] Resolves Paperclip agent identity -> Pan Persona (loads or creates soul)\n- [ ] Injects budget envelope into span context (consumed by #32\'s hard-stop)\n- [ ] Injects secrets into gov.secrets scope\n- [ ] Reports structured results back to Paperclip in expected format\n- [ ] Session state persists across heartbeats via #42\'s soul/persona plugin\n- [ ] Works alongside other channels (Telegram, CLI, HTTP) in the same process\n\n## Dependencies\n- #47 (Persona)\n- #42 (soul/persona — session state per Persona)\n- #32 (gov.policy — budget envelope hook)\n- #34 (gov.secrets — secret injection)\n- #44 (heartbeat admission filter — heartbeat coalescing)\n- Waves 0-4 fully complete\n\n## Risk\nMedium — this is a contract with an external system. Pin the adapter interface against Paperclip\'s adapter contract early.',
    ['wave-5-assistant', 'plugin', 'sprint-5']
)

print()
print('=== AMENDMENTS (comments on existing issues) ===')

amendments = {}

amendments[6] = (
    '## Delta: Persona-scoped handle production (not once-at-provision)\n\n'
    'This issue is sharpened by the Persona concept (#47). '
    'Handles are no longer granted once at plugin provision and reused across all identities. '
    'Instead, a plugin that owns identity-scoped state exposes a *per-span factory* the core '
    'invokes at span-open: "produce a handle for Persona A." What comes back is bound to A.\n\n'
    '### Additional acceptance criteria (add to existing list)\n'
    '- [ ] The wiring registry can request a Persona-scoped handle from a plugin at span-open, '
    'given a PersonaId\n'
    '- [ ] The smallest proof: a memory-like stub plugin produces a read-only MemoryQuery bound '
    'to Persona A; a compile-time guarantee that the handle exposes no write method; and a test '
    'that a handle bound to A cannot be used to read B\'s data (because the binding, not an '
    'argument, carries the identity)\n'
    '- [ ] Handles for stateless/global plugins (e.g. cap.http) may remain provision-time-granted; '
    'only identity-scoped families use the per-span factory\n\n'
    '### Risk update\n'
    'This is the genuine Wave-0 unknown flagged in the spec. '
    'Build this proof first within Wave 0, before other lifecycle work.'
)

amendments[7] = (
    '## Delta: Persona span boundary\n\n'
    'Add a correctness assertion: the abandon-path operates within a single Persona\'s span. '
    'A superseded A-goal abandons an A-decision; it never crosses Persona boundaries.\n\n'
    '### Additional acceptance criteria\n'
    '- [ ] Confirm: a Goal revision supersession only affects decisions within the same Persona\'s span\n'
    '- [ ] Test: Persona A goal v1 starts processing, Persona B goal arrives — A v1 is NOT abandoned '
    '(different Persona)\n'
    '- [ ] Test: Persona A goal v1 starts processing, Persona A goal v2 arrives — A v1 IS abandoned'
)

amendments[8] = (
    '## Delta: Multi-Persona isolation in exit test\n\n'
    '### Additional acceptance criteria\n'
    '- [ ] The exit test drives a span for an explicit Persona A\n'
    '- [ ] A second span for Persona B is created\n'
    '- [ ] Assert that the two spans get distinct, non-overlapping state/memory handles\n'
    '- [ ] Assert that writing state for Persona A does not affect Persona B\'s state\n\n'
    'This makes multi-identity isolation a Wave-0 gate, not a Wave-5 discovery.'
)

amendments[9] = (
    '## Delta: Provider is identity-agnostic\n\n'
    'The provider receives the span (and thus the active Persona) but providers are '
    'identity-agnostic in logic. The Persona affects *which context/memory* is assembled '
    '(upstream, in the observe phase), not how the provider reasons. '
    'This keeps providers from accreting identity logic.\n\n'
    'No additional acceptance criteria needed — this is a design note for implementation.'
)

amendments[16] = (
    '## Delta: Persona-keyed state\n\n'
    '### Additional acceptance criteria\n'
    '- [ ] state.memory is keyed by Persona — two Personas in the same process have '
    'separate in-memory state\n'
    '- [ ] Even this trivial in-memory state plugin respects the persona-scoped handle contract '
    'from #6, so the pattern is exercised from the first wave\n\n'
    'Test: create state for Persona A, read from Persona B — B does not see A\'s data.'
)

amendments[25] = (
    '## Delta: Persona-scoped Ragamuffin client\n\n'
    'This is where the persona-binding becomes concrete.\n\n'
    '### Additional acceptance criteria\n'
    '- [ ] memory.vector implements the per-span persona-scoped handle factory from #6\n'
    '- [ ] Given Persona A, it returns a MemoryQuery whose retrieves are filtered to A\'s scope\n'
    '- [ ] Pan does not invent the identity; it scopes to the identity the span carries\n'
    '- [ ] Retrieval for Persona A never returns Persona B\'s facts (test with two seeded Personas)\n'
    '- [ ] The identity filter is carried by the handle binding, not passed by the caller\n'
    '- [ ] Transport classification: if Ragamuffin is reached over HTTP/gRPC, this is an '
    'RPC-class plugin and falls under the runtime-enforcement regime (schema + sandbox + tokens), '
    'not the in-process compiler regime — note this explicitly in the plugin\'s architecture doc'
)

amendments[26] = (
    '## Delta: Persona-scoped MemoryQuery\n\n'
    '### Additional acceptance criteria\n'
    '- [ ] context.memory holds the persona-scoped MemoryQuery (not a global one), '
    'obtained per-span from the factory\n'
    '- [ ] Confirms the read path is identity-correct end-to-end\n\n'
    'Test: Persona A stores fact, context.memory for Persona A retrieves it, '
    'context.memory for Persona B does not.'
)

amendments[32] = (
    '## Delta: Budget envelope hard-stop\n\n'
    '### Additional acceptance criteria\n'
    '- [ ] The govern stage accepts a budget envelope carried on the span context\n'
    '- [ ] The envelope is injected from above (Paperclip in the Paperclip deployment)\n'
    '- [ ] When the envelope is exhausted, further Invokes are Denied with reason '
    '"budget exhausted"\n'
    '- [ ] This mirrors Paperclip\'s hard-stops at per-tool-call granularity\n'
    '- [ ] In non-Paperclip deployments, the envelope may be unbounded (default: unlimited)\n\n'
    '### Budget envelope format\n'
    'TBD against Paperclip\'s interface. Minimum: a token budget (e.g., 10000 tokens) '
    'that decrements on each provider call per span.'
)

amendments[34] = (
    '## Delta: Per-Persona secret resolution\n\n'
    '### Additional acceptance criteria\n'
    '- [ ] Secrets are resolved per-Persona scope\n'
    '- [ ] gov.secrets receives the active Persona from the span and resolves credentials '
    'against that Persona\'s secret store\n'
    '- [ ] Persona A cannot resolve Persona B\'s secrets\n\n'
    'Test: two Personas, each with different API keys — each resolves only its own.'
)

amendments[35] = (
    '## Delta: Persona-tagged audit events\n\n'
    '### Additional acceptance criteria\n'
    '- [ ] Every audit event (gov.audit) is tagged with the active Persona\n'
    '- [ ] The event stream Pan emits can be consumed as per-identity audit trails upstream\n'
    '- [ ] Audit query/filter supports filtering by Persona\n\n'
    'Test: emit events for Persona A and Persona B — filter by A returns only A\'s events.'
)

amendments[42] = (
    '## Delta: Per-Persona soul state\n\n'
    'Clarification: this issue IS the per-Persona soul/state implementation.\n\n'
    '### Additional acceptance criteria\n'
    '- [ ] Soul/state is keyed by Persona\n'
    '- [ ] One process holds many souls concurrently, each loaded and persisted under '
    'its Persona\'s scope\n'
    '- [ ] This is the durable side of the Wave-0 Persona binding (#47) — '
    'state.file (#18) stores each soul under its Persona\'s key'
)

amendments[43] = (
    '## Delta: Per-Persona scheduling\n\n'
    '### Additional acceptance criteria\n'
    '- [ ] sched.cron operates per-Persona — a tick is scoped to the Persona it wakes\n'
    '- [ ] The schedule config includes the target Persona for each entry\n'
    '- [ ] Multiple Personas can have independent schedules in the same process'
)

amendments[44] = (
    '## Delta: Persona-scoped heartbeat admission\n\n'
    '### Additional acceptance criteria\n'
    '- [ ] The heartbeat admission filter operates within each Persona\'s context\n'
    '- [ ] "Has anything changed?" is evaluated per-Persona, not globally\n'
    '- [ ] Persona A\'s tick does not wake Persona B if Persona B has nothing changed\n'
    '- [ ] A tick without its Persona binding is rejected at admission'
)

for num, body in sorted(amendments.items()):
    cid = add_comment(num, body)
    if cid != 'ERR':
        print('  Comment added to #' + str(num))
    else:
        print('  ERROR on #' + str(num))
    time.sleep(0.35)

print()
print('DONE — 3 new issues created, ' + str(len(amendments)) + ' amendments posted as comments')
