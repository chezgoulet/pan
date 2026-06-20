#!/usr/bin/env python3
import subprocess, json, time, sys

# Read token without any string interpolation issues
tok_file = open('/opt/data/.github_token')
token = tok_file.read().strip()
tok_file.close()

base = 'https://api.github.com/repos/chezgoulet/pan'

def ci(title, body, labels_list):
    data = json.dumps({'title': title, 'body': body, 'labels': labels_list})
    req = subprocess.run(
        ['curl', '-s', '-X', 'POST',
         '-H', 'Authorization: Bearer ' + token,
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

# ===== WAVE 1 =====
print('=== WAVE 1 - Walking Skeleton (CLI Agent) ===')

ci(
    'Implement provider.llm.anthropic - first real provider plugin',
    '## Description\nThe Anthropic Claude LLM provider. Maps Goal + Context + capabilities into Anthropic API calls, parses responses into ActionIntents.\n\nKey: everything chat-shaped stays inside this plugin. Provider trait knows none of it.\n\n## Acceptance Criteria\n- [ ] Provider trait impl for Anthropic API\n- [ ] API key from config, not hardcoded\n- [ ] Maps Goal.trigger to conversational context\n- [ ] Maps Capabilities to Anthropic tool definitions\n- [ ] Handles: text to Express, tool_use to Invoke, stop_reason to Conclude\n- [ ] Configurable model name\n- [ ] Unit tests with mock HTTP\n\n## Dependencies: W0-6 (plugin lifecycle). Risk: Low',
    ['wave-1-skeleton', 'plugin', 'sprint-1']
)

ci(
    'Implement cap.registry - capability registration and resolution',
    '## Description\nThe capability registry. Capabilities register with name (hierarchical ID), schemas, permission class, transport. Pipeline resolve stage reads from it.\n\n## Acceptance Criteria\n- [ ] Register: id, summary, args_schema, permission_class, transport\n- [ ] Lookup by hierarchical ID\n- [ ] Unknown capability to ResolutionError\n- [ ] Unit tests\n\n## Dependencies: W0-6. Risk: Low',
    ['wave-1-skeleton', 'plugin', 'sprint-1']
)

ci(
    'Implement gov.allow - trivial always-allow governance plugin',
    '## Description\nPlaceholder governance that always returns Allow. Lets pipeline run while real governance is built in Wave 4.\n\n## Acceptance Criteria\n- [ ] Governance trait impl (always Allow)\n- [ ] Plugin lifecycle: Register as gov.allow\n- [ ] Test: any intent to Allow\n\n## Dependencies: W0-6. Risk: None',
    ['wave-1-skeleton', 'plugin', 'sprint-1']
)

ci(
    'Implement exec.local - in-process capability execution',
    '## Description\nIn-process execution. Function call. Fast trusted capabilities. Dangerous ones move to exec.docker in Wave 4.\n\n## Acceptance Criteria\n- [ ] Execution trait impl\n- [ ] Capability runs as function call\n- [ ] Timeout support\n- [ ] Test: success, panic caught as error\n\n## Dependencies: W0-6. Risk: Low',
    ['wave-1-skeleton', 'plugin', 'sprint-1']
)

ci(
    'Implement cap.shell - shell command execution capability',
    '## Description\nFirst real capability: run a shell command. Intentionally dangerous for development. Wave 4 gates behind approval and Docker sandbox.\n\n## Acceptance Criteria\n- [ ] Registers as cap.shell\n- [ ] Accepts { command: String }\n- [ ] Validates args (non-empty, length limit)\n- [ ] Returns stdout, stderr, exit code\n- [ ] Timeout (30s default)\n- [ ] Tests: success, nonexistent command\n\n## Dependencies: W1-4 (exec.local), W1-2 (cap.registry). Risk: Low-Medium',
    ['wave-1-skeleton', 'plugin', 'sprint-1']
)

ci(
    'Implement obs.logging - structured observation logging',
    '## Description\nStructured logging via tracing crate. Without this, development is blind.\n\n## Acceptance Criteria\n- [ ] Observation trait impl\n- [ ] Structured log output (JSON)\n- [ ] Log levels: trace (events), debug (pipeline), info (decisions), error (errors)\n- [ ] Configurable log level\n\n## Dependencies: W0-5 (event stream). Risk: None',
    ['wave-1-skeleton', 'plugin', 'sprint-1']
)

ci(
    'Implement channel.cli - stdin/stdout CLI channel',
    '## Description\nSimplest human interface. stdin lines to observations, Express body to stdout.\n\n## Acceptance Criteria\n- [ ] Channel trait impl\n- [ ] Reads stdin lines to Goal::Utterance\n- [ ] Express body to stdout\n- [ ] EOF to clean termination\n\n## Dependencies: W0-4 (loop). Risk: Low',
    ['wave-1-skeleton', 'plugin', 'sprint-1']
)

ci(
    'Implement state.memory - in-memory non-persistent state',
    '## Description\nIn-memory state store for commit phase. Non-persistent. Replaced by state.file in Wave 2.\n\n## Acceptance Criteria\n- [ ] StateSlot trait impl\n- [ ] Load/persist in-memory\n- [ ] Thread-safe\n- [ ] Test: persist then load returns same bytes\n\n## Dependencies: W0-4 (loop). Risk: None',
    ['wave-1-skeleton', 'plugin', 'sprint-1']
)

ci(
    'Wave 1 exit test: Pan CLI agent works end-to-end',
    '## Description\nWire all Wave 1 plugins into a bin crate. Type a request, model decides, shell command runs, reply prints.\n\n## Test: echo "list files in /tmp" | target/release/pan\n- Reads stdin to observation\n- Anthropic decides: Invoke(cap.shell, {command: "ls /tmp"})\n- Pipeline: resolve, validate, govern(allow), execute, record\n- Express response printed to stdout\n- Events visible in structured logs\n\n## Dependencies: All W1-* issues. Risk: Low (integration)',
    ['wave-1-skeleton', 'test', 'sprint-1']
)

# ===== WAVE 2 =====
print()
print('=== WAVE 2 - Make It Real ===')

ci(
    'Implement state.file - disk-persistent state storage',
    '## Description\nPersistent file-based state store. State/soul bytes survive restarts. Single-writer per state kind (for now).\n\n## Acceptance Criteria\n- [ ] StateSlot trait impl\n- [ ] File storage (path configurable)\n- [ ] Atomic writes (temp file + rename)\n- [ ] Single-writer lock\n- [ ] Test: persist, restart, load returns same bytes\n\n## Dependencies: W1-8 (state.memory - same trait, swap impl). Risk: Low',
    ['wave-2-real', 'plugin', 'sprint-2']
)

ci(
    'Implement cap.fs - file system read/write capability',
    '## Description\nFile system capability. Read, write, list files. Governed like any other capability.\n\n## Acceptance Criteria\n- [ ] Registers as cap.fs\n- [ ] Operations: read(path), write(path,content), list(directory)\n- [ ] Path traversal protection\n- [ ] Accepts args matching { operation, path, body? }\n- [ ] Returns file content or listing\n- [ ] Test: path escape attempt is denied\n\n## Dependencies: W1-4 (exec.local), W1-2 (cap.registry). Risk: Low',
    ['wave-2-real', 'plugin', 'sprint-2']
)

ci(
    'Implement cap.http - outbound HTTP request capability',
    '## Description\nOutbound HTTP. GET, POST, PUT, DELETE. Governed like any capability.\n\n## Acceptance Criteria\n- [ ] Registers as cap.http\n- [ ] Methods: GET, POST, PUT, DELETE\n- [ ] Accepts { method, url, headers?, body? }\n- [ ] Returns { status, headers, body }\n- [ ] Timeout (30s default)\n- [ ] Test: GET returns response\n\n## Dependencies: W1-2 (cap.registry). Risk: Low',
    ['wave-2-real', 'plugin', 'sprint-2']
)

ci(
    'Implement cap.mcp - MCP server bridge (highest-leverage plugin)',
    '## Description\nBridge to any MCP server. Discovers tools, registers each as a Pan capability. One plugin inherits the entire MCP tool ecosystem.\n\n## Acceptance Criteria\n- [ ] Registers as cap.mcp\n- [ ] Connects to MCP server (configurable endpoint)\n- [ ] Discovers tools on startup\n- [ ] Each tool registered as cap.mcp.<tool_name>\n- [ ] Forwards Invoke calls, returns results\n- [ ] Reconnection on server restart\n- [ ] Test: connect, discover, invoke\n\n## Dependencies: W1-2 (cap.registry), W1-4 (exec.local). Risk: Medium (MCP evolving)',
    ['wave-2-real', 'plugin', 'sprint-2']
)

ci(
    'Implement cap.state_write - unified state/soul write capability',
    '## Description\nUnified state-write capability. Replaces separate Mutate concept. State changes are Invokes, gated by governance same as any effect.\n\n## Acceptance Criteria\n- [ ] Registers as cap.state_write\n- [ ] Accepts { path, value }\n- [ ] Writes through current StateSlot\n- [ ] Test: Invoke state_write -> state changes -> commit persists\n- [ ] Test: governance deny -> state write refused\n\n## Dependencies: W2-1 (state.file), W1-2 (cap.registry). Risk: Low',
    ['wave-2-real', 'plugin', 'sprint-2']
)

ci(
    'Implement context.template - prompt assembly from templates',
    '## Description\nTemplate-based context assembly. System prompt, examples, current context from markdown templates. User-editable Ring 2 surface.\n\n## Acceptance Criteria\n- [ ] Registers as context.template\n- [ ] Accepts markdown template files\n- [ ] Variable substitution: {{ var_name }}\n- [ ] System prompt = template + Goal + Context\n- [ ] Test: template with variables -> correct output\n\n## Dependencies: W0-4 (loop - context phase). Risk: Low',
    ['wave-2-real', 'plugin', 'sprint-2']
)

ci(
    'Implement context.history - conversation history with pruning',
    '## Description\nOrdered history of turns. Prunes by token count or message count when limits exceeded.\n\n## Acceptance Criteria\n- [ ] Registers as context.history\n- [ ] Stores ordered (trigger, response) pairs\n- [ ] Prunes by token count (configurable, default 4000)\n- [ ] Prunes by message count (configurable, default 50)\n- [ ] Returns pruned history as context fragments\n- [ ] Test: 100 messages -> pruned to limit\n\n## Dependencies: W0-4 (loop - context phase). Risk: Low',
    ['wave-2-real', 'plugin', 'sprint-2']
)

# ===== WAVE 3 =====
print()
print('=== WAVE 3 - Memory & Honesty ===')

ci(
    'Implement memory.vector - thin client to vector store (Ragamuffin slot)',
    '## Description\nVector memory plugin. In-memory store for dev, swappable to Ragamuffin client for production. The durable cross-run facts layer.\n\n## Acceptance Criteria\n- [ ] Registers as memory.vector\n- [ ] In-memory store (instant-distance based)\n- [ ] store(text, metadata) -> vector id\n- [ ] search(query, limit) -> ranked results\n- [ ] Metadata filtering (NPC ID, user ID)\n- [ ] Ragamuffin client adapter trait (not implemented, just the trait)\n- [ ] Test: store fact, search for it, found\n\n## Dependencies: W0-6 (plugin lifecycle). Risk: Low',
    ['wave-3-memory', 'plugin', 'sprint-3']
)

ci(
    'Implement context.memory - memory retrieval context plugin',
    '## Description\nThe "it remembers me" plugin. Holds read-only MemoryQuery handle, retrieves facts, injects into provider context. Compiled-guaranteed read-only handle.\n\n## Acceptance Criteria\n- [ ] Registers as context.memory\n- [ ] Holds Arc<dyn MemoryQuery> (read-only)\n- [ ] Queries memory on each observation\n- [ ] Injects top-k as context fragments\n- [ ] Configurable count (default 5)\n- [ ] Compile-time: handle has NO write method\n- [ ] Test: store fact -> new observation -> fact appears in context\n\n## Dependencies: W3-1 (memory.vector), W0-6 (handle injection). Risk: Low',
    ['wave-3-memory', 'plugin', 'sprint-3']
)

ci(
    'Implement memory.summarizer - context condensation into durable summaries',
    '## Description\nSummarizes old context into durable memory. When history grows past threshold, extracts key facts and stores via memory.vector.\n\n## Acceptance Criteria\n- [ ] Registers as memory.summarizer\n- [ ] Triggers on history threshold\n- [ ] Extracts key facts from old window\n- [ ] Stores summaries via memory.vector\n- [ ] Uses LLM or separate model for summarization\n- [ ] Test: accumulate, summarize fires, summaries in memory\n\n## Dependencies: W3-1 (memory.vector), W2-7 (context.history), W1-1 (provider.llm). Risk: Low',
    ['wave-3-memory', 'plugin', 'sprint-3']
)

ci(
    'Implement context.compaction - compress context window when full',
    '## Description\nContext window management. When assembled context exceeds provider token limit, replaces oldest entries with summarized version.\n\n## Acceptance Criteria\n- [ ] Registers as context.compaction\n- [ ] Triggers on token threshold\n- [ ] Replaces oldest N messages with summary\n- [ ] Configurable strategy: drop oldest, summarize, etc.\n- [ ] Test: fill context -> compaction fires -> fits limit\n\n## Dependencies: W2-7 (context.history), W3-3 (memory.summarizer). Risk: Low',
    ['wave-3-memory', 'plugin', 'sprint-3']
)

ci(
    'Implement provider.litellm - multi-model provider via LiteLLM proxy',
    '## Description\nLiteLLM provider. One plugin, many models. The model-swap freedom that makes self-hosted Pan viable.\n\n## Acceptance Criteria\n- [ ] Registers as provider.llm.litellm\n- [ ] Connects to LiteLLM proxy (configurable URL)\n- [ ] Model name configurable\n- [ ] Same mapping: text->Express, tool_use->Invoke, stop->Conclude\n- [ ] Test with mock LiteLLM\n\n## Dependencies: W0-2 (Provider trait), W0-6. Risk: Low',
    ['wave-3-memory', 'plugin', 'sprint-3']
)

ci(
    'Implement provider.behaviortree - THE non-LLM honesty check',
    '## Description\nTHE honesty check. A behavior-tree provider emitting ActionIntents without any LLM. Built now to prove core never became LLM-only. If it cannot cleanly emit the same variants, something leaked - fix now.\n\n## Acceptance Criteria\n- [ ] Registers as provider.behaviortree\n- [ ] Tree nodes: Action, Condition, Sequence, Selector\n- [ ] Action node -> ActionIntent::Invoke (same type as LLM tool_use)\n- [ ] Succeed/Fail -> ActionIntent::Conclude (same type)\n- [ ] Never emits Express (pure control)\n- [ ] Invoke has correlation=None (not fabricated)\n- [ ] Test: tree stored in same Vec<Box<dyn Provider>> as LLM -> compiles\n\n## Dependencies: W0-2 (Provider trait, ActionIntent). Risk: Low (leak test already proves type level)',
    ['wave-3-memory', 'plugin', 'sprint-3']
)

ci(
    'Implement provider.rules - rule-based provider (second non-LLM)',
    '## Description\nSecond non-LLM provider. Rule engine: evaluate conditions, fire actions. Seed of heartbeat-filter logic.\n\n## Acceptance Criteria\n- [ ] Registers as provider.rules\n- [ ] Rule format: { when: condition, then: action }\n- [ ] Conditions: signal thresholds, pattern matches, state checks\n- [ ] Fires first matching rule\n- [ ] Test: signal over threshold -> Invoke\n- [ ] Test: no match -> Conclude(Continue)\n\n## Dependencies: W0-2 (Provider trait). Risk: Low',
    ['wave-3-memory', 'plugin', 'sprint-3']
)

# ===== WAVE 4 =====
print()
print('=== WAVE 4 - Governance ===')

ci(
    'Implement gov.policy - policy-based allow/deny/approve rules',
    '## Description\nReplace gov.allow with real policy engine. Rules evaluate Invoke against capability, arguments, caller, time. Default deny.\n\n## Acceptance Criteria\n- [ ] Registers as gov.policy (replaces gov.allow)\n- [ ] Rule format: { match: { capability, args? }, action: Allow|Deny|RequireApproval }\n- [ ] First decisive rule wins\n- [ ] Default deny must explicitly allow\n- [ ] Tests: allow, deny, require-approval, no-match\n\n## Dependencies: W0-3 (pipeline govern stage), W1-3 (gov.allow - replaces). Risk: Low',
    ['wave-4-governance', 'plugin', 'sprint-4']
)

ci(
    'Implement gov.approval - human-in-the-loop for dangerous invokes',
    '## Description\nWhen governance says RequireApproval, invocation is deferred pending human approval within timeout.\n\n## Acceptance Criteria\n- [ ] ApprovalHandler impl\n- [ ] Deferred invocation notification\n- [ ] Approval timeout (configurable, default 5m)\n- [ ] Approved -> execute, Denied -> skip, Timeout -> skip\n- [ ] Notification: capability name, args, context\n- [ ] Tests: approve, timeout\n\n## Dependencies: W4-1 (gov.policy can return RequireApproval). Risk: Low',
    ['wave-4-governance', 'plugin', 'sprint-4']
)

ci(
    'Implement gov.secrets - resolve credentials without exposing to plugins',
    '## Description\nSecret resolution. API keys and credentials stored encrypted, resolved at govern stage. Never exposed to plugins.\n\n## Acceptance Criteria\n- [ ] Registers as gov.secrets\n- [ ] Secrets in encrypted config\n- [ ] Credential resolved if governance allows\n- [ ] Secret injected at execute stage, invisible to provider\n- [ ] Test: authorized cap gets key, unauthorized cap denied\n\n## Dependencies: W0-3 (govern stage). Risk: Low',
    ['wave-4-governance', 'plugin', 'sprint-4']
)

ci(
    'Implement gov.audit - durable record of every governed effect',
    '## Description\nDurable audit log. Every Invoke recorded: capability, args, governance result, execution result, timestamp.\n\n## Acceptance Criteria\n- [ ] Registers as gov.audit\n- [ ] Records every Invoke attempt with full metadata\n- [ ] Storage: durable EventSink (file-based)\n- [ ] Query: filter by capability, result, time\n- [ ] Tests: allowed recorded, denied recorded\n\n## Dependencies: W0-5 (event stream - durable mode), W0-3 (pipeline). Risk: Low',
    ['wave-4-governance', 'plugin', 'sprint-4']
)

ci(
    'Implement gov.ratelimit - token/request/action ceilings',
    '## Description\nRate limiting: requests/min, tokens/min, actions/min. Prevents runaway agent.\n\n## Acceptance Criteria\n- [ ] Registers as gov.ratelimit\n- [ ] Configurable: requests/minute, tokens/minute, actions/minute\n- [ ] Per-user/quota (for future multi-tenant)\n- [ ] Sliding window counters\n- [ ] Tests: within limit->Allow, exceeded->Deny\n\n## Dependencies: W0-3 (govern stage). Risk: Low',
    ['wave-4-governance', 'plugin', 'sprint-4']
)

ci(
    'Implement gov.idempotency - deduplicate repeated invocations',
    '## Description\nPrevents same invocation from executing multiple times. Tracks by (capability, args_hash, goal_id).\n\n## Acceptance Criteria\n- [ ] Registers as gov.idempotency\n- [ ] Tracks recent invokes by (cap, args_hash, goal_id)\n- [ ] Duplicate within window -> Deny\n- [ ] Tests: same invoke twice -> second denied, different args -> both allowed\n\n## Dependencies: W0-3 (govern stage). Risk: Low',
    ['wave-4-governance', 'plugin', 'sprint-4']
)

ci(
    'Implement exec.docker - sandboxed container execution',
    '## Description\nSandboxed execution via Docker. Dangerous capabilities (shell, fs) run in container instead of host process.\n\n## Acceptance Criteria\n- [ ] Registers as exec.docker\n- [ ] Configurable container image\n- [ ] Each invocation -> new ephemeral container\n- [ ] Host filesystem isolation\n- [ ] Network access configurable (default: none)\n- [ ] Timeout and cleanup\n- [ ] Capabilities declare preferred executor (local vs docker)\n- [ ] Tests: shell runs in docker, exit code forwarded\n\n## Dependencies: W0-3 (execute stage selects transport). Risk: Medium (Docker edge cases)',
    ['wave-4-governance', 'plugin', 'sprint-4']
)

# ===== WAVE 5 =====
print()
print('=== WAVE 5 - Hermes/OpenClaw Replacement ===')

ci(
    'Implement channel.telegram - Telegram bot channel',
    '## Description\nTelegram Bot API integration. Inbound messages to observations, Express responses as replies.\n\n## Acceptance Criteria\n- [ ] Registers as channel.telegram\n- [ ] Telegram Bot API via long polling or webhook\n- [ ] Inbound messages to Goal::Utterance\n- [ ] Express body to Telegram message\n- [ ] Markdown formatting\n\n## Dependencies: W0-4 (loop). Risk: Low',
    ['wave-5-assistant', 'plugin', 'sprint-5']
)

ci(
    'Implement channel.http - HTTP webhook/REST ingress',
    '## Description\nHTTP server for webhooks and REST. POST requests to observations, responses via HTTP.\n\n## Acceptance Criteria\n- [ ] Registers as channel.http\n- [ ] HTTP server (configurable port)\n- [ ] POST /webhook to Goal::Event\n- [ ] POST /chat to Goal::Utterance, returns Express body\n- [ ] Webhook signature verification\n- [ ] Test: curl POST -> observation -> response\n\n## Dependencies: W0-4 (loop). Risk: Low',
    ['wave-5-assistant', 'plugin', 'sprint-5']
)

ci(
    'Add pairing/allowlist rules to gov.policy for inbound channels',
    '## Description\nBy default inbound channels are untrusted. Only paired senders reach the agent. Non-paired get "not authorized" without LLM cost.\n\n## Acceptance Criteria\n- [ ] Pairing mechanism: user sends code, agent authorizes\n- [ ] Allowlist rule in gov.policy\n- [ ] Unpaired user -> Express("not authorized"), no LLM call\n- [ ] Tests: paired normal, unpaired denied no LLM cost\n\n## Dependencies: W4-1 (gov.policy). Risk: Low',
    ['wave-5-assistant', 'plugin', 'sprint-5']
)

ci(
    'Implement soul/persona plugin - persistent agent identity',
    '## Description\nPersistent identity: name, personality, preferences, relationship state. User-editable markdown (Ring 2). Combined with context.template = the "make it yours" onboarding.\n\n## Acceptance Criteria\n- [ ] State-handle plugin\n- [ ] Soul file: TOML/YAML with name, personality, preferences\n- [ ] Loaded before first observation, persisted after commit\n- [ ] Personality injected into context.template\n- [ ] User-editable markdown, not Rust changes\n\n## Dependencies: W2-1 (state.file), W2-6 (context.template). Risk: Low',
    ['wave-5-assistant', 'plugin', 'sprint-5']
)

ci(
    'Implement sched.cron and sched.eventbus - scheduling plugins',
    '## Description\nsched.cron: cron-like scheduling, fires Tick observations. sched.eventbus: in-process event subscriptions, fires Event observations.\n\n## Acceptance Criteria\n- [ ] sched.cron: cron expressions, fires Goal::Tick\n- [ ] sched.eventbus: event handlers, fires Goal::Event\n- [ ] Configurable schedule\n\n## Dependencies: W0-4 (loop). Risk: Low',
    ['wave-5-assistant', 'plugin', 'sprint-5']
)

ci(
    'Implement heartbeat admission filter - cheap observations usually dropped',
    '## Description\nTHE fix for "wakes the whole agent every 30 min". Admission plugin: most ticks dropped without reaching provider. Only escalates if watched condition changed.\n\n## Mechanism\n- sched.cron fires Tick\n- Admission checks: anything different since last tick?\n- State change: last activity timestamp, watched values\n- Nothing changed -> drop (no provider call)\n- Something changed -> admit\n\n## Acceptance Criteria\n- [ ] Admission plugin in observe phase\n- [ ] Receives Tick observations\n- [ ] State change detection (last-active, watched values)\n- [ ] Configurable interval\n- [ ] Tests: consecutive ticks mostly dropped, state change admits\n\n## Dependencies: W5-5 (sched.cron), W0-4 (loop). Risk: Medium (heuristics tuning)',
    ['wave-5-assistant', 'plugin', 'sprint-5']
)

ci(
    'Implement skill.runner - agentskills.io polyglot skill execution',
    '## Description\nRing 2 surface. Executes agentskills.io-format skills. Polyglot: shell, Python, anything. Runs sandboxed via exec.docker.\n\n## Acceptance Criteria\n- [ ] Registers as skill.runner\n- [ ] Skill format: markdown with metadata + executable section\n- [ ] Discoverable: directory scanned, each skill becomes a capability\n- [ ] Polyglot: any installed interpreter\n- [ ] Sandboxed via exec.docker\n- [ ] Test: write shell skill, Pan discovers and runs it\n\n## Dependencies: W4-7 (exec.docker), W1-2 (cap.registry). Risk: Low',
    ['wave-5-assistant', 'plugin', 'sprint-5']
)

ci(
    'Implement cap.distribution - scope which capabilities are live per deployment',
    '## Description\nScopes available capabilities per deployment profile. NPC deployment doesnt get cap.shell. Chat deployment doesnt get npc.move. Same binary, different scopes.\n\n## Acceptance Criteria\n- [ ] Registers as cap.distribution\n- [ ] Configurable allowlist of capability IDs\n- [ ] Capabilities not in allowlist never registered\n- [ ] Profiles: chat (shell,fs,http,mcp), NPC (move,dialogue,memory), trend (rules,alert)\n- [ ] Test: chat profile has cap.shell, not npc.move\n\n## Dependencies: W1-2 (cap.registry). Risk: Low',
    ['wave-5-assistant', 'plugin', 'sprint-5']
)

print()
print('DONE')
print('Total issues created across Waves 1-5 (Wave 0 already done in earlier batch)')
