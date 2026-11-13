# Nightjar — Crypto Forensics

A scoped investigation mode sharing Nightjar's Rust engine, case ledger, graph and human-review policy. It is not a separate fork or a wallet-identity oracle.

## Ask → Explore → Review

- **Terminal:** ask a connected MCP agent a natural-language question and explicitly supply the Ethereum seeds. Alternatively **C** opens a two-field local plan: question and one address/transaction. **Ctrl+S** saves one pending seed step without collecting. **w → Enter** selects that step; **g** attempts it once.
- **Desktop:** choose the **Crypto** scope under Ask, supply explicit Ethereum seeds and save locally. Running the external agent requires configured endpoint/model, explicit provider/charges consent and a separate collection opt-in, initially OFF.
- **Explore:** source-backed typed address/transaction nodes and directed, labeled transaction roles. **G** opens the terminal graph; **o** opens original evidence. No automatic address clustering or identity merging.
- **Review:** all analytical findings stay `pending_review`. In the local TUI, select a finding, press **r**, enter an explicit decision and rationale, then **Ctrl+S**. Only accepted findings enter the structured summary. Even accepted findings never become source graph edges.

Opening an interface or saving a plan does not contact a provider. An explicit collection sends the address/transaction target to the third-party explorer.

## Initial support

| Item | Support |
| --- | --- |
| Network | Ethereum mainnet only |
| Address target | `ethereum:0x` followed by exactly 40 hexadecimal digits |
| Transaction target | `ethereum:0x` followed by exactly 64 hexadecimal digits |
| Collectors | `evm-address`, `evm-transaction` |
| Source | Fixed Ethereum Blockscout REST endpoints; no supplied endpoint override or automatic fallback |
| Navigation | Address summary plus one bounded transaction page (up to two requests); transaction lookup uses one request. No automatic pagination or retries |
| Amounts | Decimal native-value **wei strings**; no floating-point conversion or fiat pricing |
| Graph identifiers | Distinct `evm_address` / `evm_transaction` types; canonical `eip155:1:0x…` values |
| Grounded pivots | Exact retained `ethereum:0x…` data strings derived from schema-validated provider identifiers; plan and run remain separate |

The requested spelling is retained separately from canonical graph identifiers. Address and transaction nodes do not collapse even if their hexadecimal text resembles another identifier. Normalized leads describe source-reported identifiers, not a fetched follow-up or independently verified attribution.

Ethereum-only validation is intentional: unsupported chains, malformed identifiers and unsupported options fail before network access. Crypto investigations allow only the two crypto collectors, and noncrypto recon modes exclude them. Ten total steps, including attached captures, is the investigation budget. A previously attempted step is not retried automatically.

## Agent example (local planning only)

The all-zero address below is a synthetic example, not an instruction to collect it.

Create through `fieldkit_investigation_create`:

```json
{"question":"What transaction roles are observable for the explicitly scoped Ethereum seed?","mode":"crypto","seeds":["ethereum:0x0000000000000000000000000000000000000000"]}
```

Plan through `fieldkit_investigation_plan_step`:

```json
{"id":"INVESTIGATION_ID","job":{"command":"evm-address","target":"ethereum:0x0000000000000000000000000000000000000000","options":{}},"rationale":"Inspect the explicit Ethereum seed, without inferring identity, ownership or criminality."}
```

Creation and planning collect nothing. Only execute `fieldkit_investigation_run_step` after collection is authorized. Inspect `fieldkit_investigation_evidence`, its coverage/status and retained sources before proposing a finding. `fieldkit_investigation_source` reads the hash-verified original response without recollection. For a follow-up, use an exact `/data/...` target string from that report with `lead:{step_id,pointer}`; do not transform the canonical graph value into an ungrounded target. Hash checks are repeated before execution.

The external Rust agent accepts `--mode crypto`; it still defaults to plan-only. The existing six scripted replay scenarios test the general harness, **not crypto investigation quality**.

## Provenance and limits

- Retained response bytes and immutable reports carry SHA-256 provenance. Citations bind exact report/source membership and values, not source truth.
- Explorer responses are provider observations, not independent Ethereum-node verification. Coverage is incomplete, bounded and not a full wallet history.
- Submitted/recipient roles can exist on pending or failed transactions. Such edges do **not** assert a successful transfer. No balance-conservation, proceeds-of-crime or beneficial-ownership conclusion follows from them.
- Explorer names/labels are attributed provider claims, not verified owners. They do not create person/entity ownership edges.
- Denied, retired, rate-limited or malformed API responses remain explicit unresolved outcomes; they are never converted into an empty history. No paid API gateway is silently substituted. Live service availability has not been validated by offline tests.
- No token-transfer/internal-call completeness, UTXO tracing, Solana/Tron, bridge resolution, mixer inference, exchange attribution, PNL or automatic criminality scoring is claimed.
- Cielo, TRM, Arkham, MetaSleuth, Dune, explorer/bridge services and other suggested tools are roadmap references, **not installed integrations**. No subscription credits, leak/infostealer databases, private accounts, spoofed dApp sessions or sensitive lookup connectors are activated.

Use explicit scope and human-reviewed interpretations. A graph is a way to inspect evidence, not proof of identity or guilt.
