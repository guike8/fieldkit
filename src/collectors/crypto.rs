//! Ethereum-only, keyless Blockscout observations; no transfer or ownership inference.
use super::*;

const BASE: &str = "https://eth.blockscout.com/api/v2";
const ADDRESS_KIND: &str = "evm_address";
const TRANSACTION_KIND: &str = "evm_transaction";

pub(super) fn coverage() -> Value {
    json!({"provider":"Blockscout Ethereum explorer","chain":"ethereum","chain_id":"eip155:1",
        "base_url":BASE,"credentials_sent":false,"provider_access_may_require_credentials":true,"live_availability_verified":false,
        "documentation":"https://docs.blockscout.com/devs/apis/requests-and-limits",
        "schema_documentation":"https://docs.blockscout.com/api-reference/transactions/retrieve-detailed-information-about-a-specific-transaction",
        "availability_caveat":"Public instance APIs may be retired, denied or rate limited; no gateway, paid key, fallback or retry is used",
        "max_address_requests":2,"max_transaction_requests":1,"max_pages":1,"max_results":50,
        "automatic_pagination":false,"automatic_retries":false,"history_complete":false,
        "scope":"Provider-reported outer Ethereum transactions only; no internal calls, token transfers, tracing, prices, complete history, identity or ownership verification. Recipient/submission roles do not establish funds transferred; confirmations are provider observations, not finality guarantees."})
}

pub(super) fn descriptors() -> Vec<Value> {
    [("evm-address", 40, "Observe a public Ethereum address summary and one bounded transaction page from fixed Blockscout ETH endpoints (at most two requests). No complete history, token/internal transfers or ownership attribution; provider labels are attributed data only."),
     ("evm-transaction", 64, "Observe one public Ethereum outer transaction from a fixed Blockscout ETH endpoint (one request). Exact raw wei and provider confirmation state; submission/recipient roles never prove transfer, identity or ownership.")]
        .into_iter().map(|(name, length, description)| {
            let options = if name == "evm-address" {
                json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":50}},"additionalProperties":false})
            } else { json!({"type":"object","properties":{},"additionalProperties":false}) };
            json!({"name":name,"description":description,"input_schema":{"type":"object","properties":{
                "command":{"type":"string","const":name},
                "target":{"type":"string","minLength":11+length,"maxLength":11+length,"pattern":format!("^ethereum:0x[A-Fa-f0-9]{{{length}}}$"),"description":"Explicit Ethereum mainnet target only; bare hashes and other chains rejected before network"},
                "options":options},"required":["command","target"],"additionalProperties":false}})
        }).collect()
}

fn valid_hash(value: &str, length: usize) -> bool {
    value.len() == length + 2
        && value.starts_with("0x")
        && value.as_bytes()[2..].iter().all(u8::is_ascii_hexdigit)
}

fn target_hash(target: &str, length: usize) -> Result<&str> {
    target.strip_prefix("ethereum:").filter(|hash| valid_hash(hash, length)).ok_or_else(|| {
        fail("invalid", "INVALID_ETHEREUM_TARGET", format!("Supply ethereum:0x followed by exactly {length} hexadecimal characters; only Ethereum mainnet is supported"))
    })
}

pub(super) fn validate(command: &str, target: &str) -> Result<()> {
    target_hash(target, if command == "evm-address" { 40 } else { 64 }).map(|_| ())
}

pub(super) fn validate_seed(target: &str) -> Result<()> {
    if target_hash(target, 40).is_ok() || target_hash(target, 64).is_ok() {
        Ok(())
    } else {
        Err(fail(
            "invalid",
            "INVALID_ETHEREUM_TARGET",
            "Use ethereum:0x{40 hex address or 64 hex transaction}; other chains and bare hashes are excluded",
        ))
    }
}

fn canonical(hash: &str) -> String {
    format!("eip155:1:{}", hash.to_ascii_lowercase())
}
fn qualified(hash: &str) -> String {
    format!("ethereum:{hash}")
}
fn schema_error() -> anyhow::Error {
    fail(
        "unknown",
        "ETHEREUM_PROVIDER_SCHEMA",
        "Blockscout response lacks valid, target-bound Ethereum observation fields",
    )
}
fn decimal(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 78 && s.bytes().all(|c| c.is_ascii_digit()))
}
fn observed_address(value: &Value) -> Result<&str> {
    value["hash"]
        .as_str()
        .filter(|s| valid_hash(s, 40))
        .ok_or_else(schema_error)
}
fn provider_fields(raw: &Value) -> Result<Value> {
    if raw
        .get("name")
        .is_some_and(|v| !v.is_null() && !v.is_string())
        || raw.get("is_contract").is_some_and(|v| !v.is_boolean())
    {
        return Err(schema_error());
    }
    Ok(json!({"provider":"Blockscout Ethereum explorer",
        "name":raw["name"].as_str().map(|s| clean(s, 500)),
        "is_contract":raw["is_contract"].as_bool(),
        "scope":"Unverified provider metadata, not person identity or ownership attribution"}))
}

fn payload(target: &str, limit: usize) -> Payload {
    Payload::new(
        json!({"input":target,"chain":"ethereum","chain_id":"eip155:1", "coverage":coverage(),
        "history_complete":false,"identity_attribution":"not_established","ownership":"not_established",
        "lead_scope":"Chain-qualified targets are normalized from retained provider hashes, not independent fetches, verified ownership or scope authorization",
        "transactions":[],"returned":0,"limit":limit,"truncated":false,"pages_fetched":0}),
    )
}
fn record_error(p: &mut Payload, error: anyhow::Error) {
    p.status = if p.entities.is_empty() {
        error_status(&error)
    } else {
        "partial"
    }
    .into();
    let mut detail = error_value(&error);
    detail["retryable"] = json!(false);
    p.errors.push(detail);
}

fn summary(p: &mut Payload, requested: &str, response: &Response) -> Result<()> {
    response.check()?;
    let raw = response.json()?;
    let hash = observed_address(&raw)?;
    if !hash.eq_ignore_ascii_case(requested) {
        return Err(schema_error());
    }
    let fields = provider_fields(&raw)?;
    p.data["address"] = json!({"hash":hash,"target":qualified(hash),"canonical":canonical(hash),
        "provider_fields":fields,"source_ids":[response.source_id],"source_pointer":"",
        "verification":"provider_observation_not_independent_verification"});
    p.entity(
        ADDRESS_KIND,
        canonical(hash),
        std::slice::from_ref(&response.source_id),
    );
    Ok(())
}

fn transaction(raw: &Value, source_id: &str, pointer: &str) -> Result<Value> {
    let hash = raw["hash"]
        .as_str()
        .filter(|s| valid_hash(s, 64))
        .ok_or_else(schema_error)?;
    let from = observed_address(&raw["from"])?;
    let to = match raw.get("to") {
        Some(Value::Null) => None, // Contract creation has no outer recipient.
        Some(value) => Some(observed_address(value)?),
        None => return Err(schema_error()),
    };
    let amount = decimal(&raw["value"]).ok_or_else(schema_error)?;
    let confirmations = raw["confirmations"].as_u64().ok_or_else(schema_error)?;
    let block = match raw.get("block_number") {
        Some(Value::Null) => None,
        Some(value) => Some(value.as_u64().ok_or_else(schema_error)?),
        None => return Err(schema_error()),
    };
    let status = match raw.get("status") {
        Some(Value::Null) => None,
        Some(value) if ["ok", "error"].contains(&value.as_str().unwrap_or("")) => value.as_str(),
        _ => return Err(schema_error()),
    };
    let result = raw["result"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 500)
        .ok_or_else(schema_error)?;
    let confirmation_status = if status == Some("error") {
        "failed"
    } else if block.is_none() || confirmations == 0 || result == "pending" {
        "pending_or_unconfirmed"
    } else if status == Some("ok") && result == "success" {
        "confirmed_success_reported"
    } else {
        "confirmed_outcome_unknown"
    };
    Ok(
        json!({"hash":hash,"target":qualified(hash),"canonical":canonical(hash),
        "from_hash":from,"from_target":qualified(from),"from_canonical":canonical(from),
        "to_hash":to,"to_target":to.map(qualified),"to_canonical":to.map(canonical),
        "amount_wei":amount,"provider_status":status,"provider_result":result,
        "confirmations":confirmations,"block_number":block,"confirmation_status":confirmation_status,
        "funds_transferred":"not_established_by_this_capture",
        "from_provider_fields":provider_fields(&raw["from"])? ,
        "to_provider_fields":to.map(|_| provider_fields(&raw["to"])).transpose()?,
        "source_ids":[source_id],"source_pointer":pointer,
        "verification":"provider_observation_not_independent_verification"}),
    )
}

fn graph_transaction(p: &mut Payload, row: &Value, sid: &str) {
    let sources = [sid.to_owned()];
    let tx = row["canonical"].as_str().unwrap();
    let from = row["from_canonical"].as_str().unwrap();
    p.entity(TRANSACTION_KIND, tx, &sources);
    p.entity(ADDRESS_KIND, from, &sources);
    // These are literal transaction roles, including for failed/pending transactions.
    // No edge says paid/transferred/owns, and no provider label becomes an entity.
    p.edge(
        ADDRESS_KIND,
        from,
        "submitted_transaction",
        TRANSACTION_KIND,
        tx,
        &sources,
    );
    if let Some(to) = row["to_canonical"].as_str() {
        p.entity(ADDRESS_KIND, to, &sources);
        p.edge(
            TRANSACTION_KIND,
            tx,
            "transaction_recipient",
            ADDRESS_KIND,
            to,
            &sources,
        );
    }
}

fn page(p: &mut Payload, requested: &str, limit: usize, response: &Response) -> Result<()> {
    response.check()?;
    let raw = response.json()?;
    let items = raw["items"].as_array().ok_or_else(schema_error)?;
    let next = match raw.get("next_page_params") {
        Some(Value::Null) => false,
        Some(Value::Object(_)) => true,
        _ => return Err(schema_error()),
    };
    let mut retained = Vec::new();
    let mut invalid = 0;
    for (index, item) in items.iter().take(limit).enumerate() {
        let row = transaction(item, &response.source_id, &format!("/items/{index}"));
        match row {
            Ok(row)
                if row["from_hash"]
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case(requested))
                    || row["to_hash"]
                        .as_str()
                        .is_some_and(|s| s.eq_ignore_ascii_case(requested)) =>
            {
                graph_transaction(p, &row, &response.source_id);
                retained.push(row);
            }
            _ => invalid += 1,
        }
    }
    p.data["pages_fetched"] = json!(1);
    p.data["provider_page_records"] = json!(items.len());
    p.data["provider_has_next_page"] = json!(next);
    p.data["truncated"] = json!(next || items.len() > limit || invalid > 0);
    p.data["invalid_records"] = json!(invalid);
    p.data["returned"] = json!(retained.len());
    p.data["transactions"] = json!(retained);
    if next || items.len() > limit || invalid > 0 {
        p.status = "partial".into();
    }
    if invalid > 0 {
        p.errors.push(json!({"code":"ETHEREUM_INVALID_RECORDS","message":"Invalid or unrelated transaction records excluded; history is incomplete","retryable":false}));
    }
    Ok(())
}

fn single(p: &mut Payload, requested: &str, response: &Response) -> Result<()> {
    response.check()?;
    let row = transaction(&response.json()?, &response.source_id, "")?;
    if !row["hash"]
        .as_str()
        .unwrap()
        .eq_ignore_ascii_case(requested)
    {
        return Err(schema_error());
    }
    graph_transaction(p, &row, &response.source_id);
    p.data["transactions"] = json!([row]);
    p.data["returned"] = json!(1);
    Ok(())
}

pub(super) fn collect(
    ctx: &mut Context<'_>,
    command: &str,
    target: &str,
    options: &Value,
) -> Result<Payload> {
    collect_with(ctx, command, target, options, |ctx, url| {
        ctx.fetch(url, false)
    })
}

fn collect_with(
    ctx: &mut Context<'_>,
    command: &str,
    target: &str,
    options: &Value,
    mut fetch: impl FnMut(&mut Context<'_>, &str) -> Result<Response>,
) -> Result<Payload> {
    let address = command == "evm-address";
    let hash = target_hash(target, if address { 40 } else { 64 })?;
    let limit = if address {
        opt_limit(options, 20, ctx.cfg.max_results.min(50))
    } else {
        ctx.cfg.max_results.min(1)
    };
    if limit == 0 {
        return Err(fail(
            "invalid",
            "INVALID_LIMIT",
            "Allow at least one Ethereum result",
        ));
    }
    let mut p = payload(target, limit);
    let result = if address {
        let url = format!("{BASE}/addresses/{hash}");
        fetch(ctx, &url)
            .and_then(|response| summary(&mut p, hash, &response))
            .and_then(|()| {
                // Exactly one provider-default page. Never follow next_page_params or retry.
                fetch(ctx, &format!("{url}/transactions"))
                    .and_then(|response| page(&mut p, hash, limit, &response))
            })
    } else {
        fetch(ctx, &format!("{BASE}/transactions/{hash}"))
            .and_then(|response| single(&mut p, hash, &response))
    };
    if let Err(error) = result {
        record_error(&mut p, error);
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDRESS: &str = "0xAa00000000000000000000000000000000000001";
    const RECIPIENT: &str = "0xbb00000000000000000000000000000000000002";
    const TX: &str = "0xCc00000000000000000000000000000000000000000000000000000000000003";

    fn tx() -> Value {
        json!({"hash":TX,"from":{"hash":ADDRESS,"name":"Unverified provider sender"},
            "to":{"hash":RECIPIENT,"name":"Unverified provider recipient"},
            "value":"100000000000000000000000000000000000001","confirmations":12,
            "block_number":20000000,"status":"ok","result":"success"})
    }
    fn response(ctx: &mut Context<'_>, url: &str, status: u16, body: Vec<u8>) -> Response {
        let source_id = ctx.source(
            "Synthetic Blockscout response",
            Some(url),
            &body,
            "http",
            json!({"http_status":status,"media_type":"application/json","request_method":"GET"}),
        );
        Response {
            url: url.into(),
            status,
            headers: BTreeMap::new(),
            body,
            source_id,
        }
    }
    fn raw_response(ctx: &mut Context<'_>, raw: Value) -> Response {
        response(ctx, BASE, 200, serde_json::to_vec(&raw).unwrap())
    }
    fn run_fixture(
        command: &str,
        target: &str,
        options: Value,
        maximum: usize,
        replies: Vec<(u16, Value)>,
    ) -> (Payload, Vec<Value>, BTreeMap<String, Vec<u8>>, Vec<String>) {
        let cfg = Config {
            max_results: maximum,
            ..Config::default()
        };
        let mut ctx = Context::new(&cfg);
        let mut replies = replies.into_iter();
        let mut urls = Vec::new();
        let p = collect_with(&mut ctx, command, target, &options, |ctx, url| {
            urls.push(url.to_owned());
            let (status, raw) = replies
                .next()
                .expect("No retry, extra request or pagination authorized by fixture");
            Ok(response(
                ctx,
                url,
                status,
                serde_json::to_vec(&raw).unwrap(),
            ))
        })
        .unwrap();
        (p, ctx.sources, ctx.blobs, urls)
    }

    #[test]
    fn fixed_endpoints_two_address_requests_one_page_and_one_transaction_request() {
        let (p, _, _, urls) = run_fixture(
            "evm-address",
            &qualified(ADDRESS),
            json!({}),
            50,
            vec![
                (200, json!({"hash":ADDRESS,"name":"Provider label"})),
                (200, json!({"items":[tx()],"next_page_params":{"hash":TX}})),
            ],
        );
        assert_eq!(
            urls,
            vec![
                format!("{BASE}/addresses/{ADDRESS}"),
                format!("{BASE}/addresses/{ADDRESS}/transactions")
            ]
        );
        assert_eq!(p.status, "partial");
        assert_eq!(p.data["pages_fetched"], 1);
        assert_eq!(p.data["truncated"], true);
        assert_eq!(p.data["provider_has_next_page"], true);
        assert_eq!(p.data["history_complete"], false);
        let (p, _, _, urls) = run_fixture(
            "evm-transaction",
            &qualified(TX),
            json!({}),
            50,
            vec![(200, tx())],
        );
        assert_eq!(urls, vec![format!("{BASE}/transactions/{TX}")]);
        assert_eq!(p.data["returned"], 1);
        assert_eq!(
            p.data["transactions"][0]["amount_wei"],
            "100000000000000000000000000000000000001"
        );
        assert_eq!(
            p.data["transactions"][0]["confirmation_status"],
            "confirmed_success_reported"
        );
        assert_eq!(p.data["input"], qualified(TX));
    }

    #[test]
    fn exact_provider_bytes_hashes_and_source_references_are_preserved() {
        let original = tx();
        let (p, sources, blobs, _) = run_fixture(
            "evm-transaction",
            &qualified(TX),
            json!({}),
            50,
            vec![(200, original.clone())],
        );
        let bytes = serde_json::to_vec(&original).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0]["sha256"], digest(&bytes));
        assert_eq!(blobs[&digest(&bytes)], bytes);
        assert_eq!(sources[0]["hash_scope"], "source_bytes");
        assert_eq!(p.data["transactions"][0]["source_pointer"], "");
        assert_eq!(
            original
                .pointer(
                    p.data["transactions"][0]["source_pointer"]
                        .as_str()
                        .unwrap()
                )
                .unwrap(),
            &original
        );
        for record in p.entities.iter().chain(p.relationships.iter()) {
            assert_eq!(record["source_ids"], json!([sources[0]["source_id"]]));
        }
        let row = &p.data["transactions"][0];
        assert_eq!(row["target"], qualified(TX));
        assert_eq!(row["from_target"], qualified(ADDRESS));
        assert_eq!(row["to_target"], qualified(RECIPIENT));
        assert_eq!(row["canonical"], canonical(TX));
        assert_eq!(p.entities[0]["type"], TRANSACTION_KIND);
        assert_eq!(p.entities[1]["type"], ADDRESS_KIND);
        assert!(
            p.entities
                .iter()
                .all(|e| e["value"].as_str().unwrap().starts_with("eip155:1:0x"))
        );
        assert_eq!(
            row["from_provider_fields"]["name"],
            "Unverified provider sender"
        );
        assert!(
            p.entities
                .iter()
                .all(|e| !e.to_string().contains("Unverified provider"))
        );
        assert!(
            p.relationships
                .iter()
                .all(|e| !e.to_string().contains("Unverified provider"))
        );
    }

    #[test]
    fn failed_pending_unknown_and_contract_creation_never_infer_transfers() {
        for (status, result, block, confirmations, expected) in [
            (json!("error"), "execution reverted", json!(20), 3, "failed"),
            (
                Value::Null,
                "pending",
                Value::Null,
                0,
                "pending_or_unconfirmed",
            ),
            (
                json!("ok"),
                "success",
                json!(20),
                0,
                "pending_or_unconfirmed",
            ),
            (
                Value::Null,
                "awaiting_internal_transactions",
                json!(20),
                3,
                "confirmed_outcome_unknown",
            ),
            (
                json!("ok"),
                "dropped/replaced",
                json!(20),
                3,
                "confirmed_outcome_unknown",
            ),
        ] {
            let mut raw = tx();
            raw["status"] = status;
            raw["result"] = json!(result);
            raw["block_number"] = block;
            raw["confirmations"] = json!(confirmations);
            let (p, _, _, _) = run_fixture(
                "evm-transaction",
                &qualified(TX),
                json!({}),
                50,
                vec![(200, raw)],
            );
            assert_eq!(p.data["transactions"][0]["confirmation_status"], expected);
            assert_eq!(
                p.data["transactions"][0]["funds_transferred"],
                "not_established_by_this_capture"
            );
            assert_eq!(p.relationships.len(), 2);
            assert_eq!(p.relationships[0]["relation"], "submitted_transaction");
            assert_eq!(p.relationships[1]["relation"], "transaction_recipient");
        }
        let mut creation = tx();
        creation["to"] = Value::Null;
        let (p, _, _, _) = run_fixture(
            "evm-transaction",
            &qualified(TX),
            json!({}),
            50,
            vec![(200, creation)],
        );
        assert_eq!(p.relationships.len(), 1);
        assert_eq!(p.data["transactions"][0]["to_target"], Value::Null);
    }

    #[test]
    fn schema_and_target_binding_fail_closed_without_graph_claims() {
        let cfg = Config::default();
        let mut ctx = Context::new(&cfg);
        for raw in [
            Value::Null,
            json!({}),
            json!([]),
            json!({"message":"API deprecated"}),
            json!({"hash":TX}),
        ] {
            let response = raw_response(&mut ctx, raw);
            let mut p = payload(&qualified(TX), 1);
            assert!(single(&mut p, TX, &response).is_err());
            assert!(p.entities.is_empty());
            assert!(p.relationships.is_empty());
        }
        for (key, value) in [
            ("hash", json!(ADDRESS)),
            ("hash", json!(format!("0x{}", "d".repeat(64)))),
            ("from", json!({"hash":TX})),
            ("to", json!({"hash":"ethereum:nothex"})),
            ("value", json!(123)),
            ("value", json!(1.5)),
            ("value", json!("1e18")),
            ("value", json!("-1")),
            ("value", json!("")),
            ("value", json!("1".repeat(79))),
            ("confirmations", json!(-1)),
            ("confirmations", json!("1")),
            ("confirmations", json!(1.5)),
            ("block_number", json!("20")),
            ("block_number", json!(-1)),
            ("status", json!("success")),
            ("status", json!(false)),
            ("result", Value::Null),
        ] {
            let mut raw = tx();
            raw[key] = value;
            let mut p = payload(&qualified(TX), 1);
            assert!(
                single(&mut p, TX, &raw_response(&mut ctx, raw.clone())).is_err(),
                "accepted {raw}"
            );
            assert!(p.entities.is_empty());
        }
        for key in [
            "hash",
            "from",
            "to",
            "value",
            "confirmations",
            "block_number",
            "status",
            "result",
        ] {
            let mut raw = tx();
            raw.as_object_mut().unwrap().remove(key);
            assert!(transaction(&raw, "src", "/").is_err(), "missing {key}");
        }
        let mut p = payload(&qualified(ADDRESS), 1);
        assert!(
            summary(
                &mut p,
                ADDRESS,
                &raw_response(&mut ctx, json!({"hash":RECIPIENT}))
            )
            .is_err()
        );
        assert!(p.entities.is_empty());
        let bad_json = response(&mut ctx, BASE, 200, b"not JSON".to_vec());
        assert!(single(&mut p, TX, &bad_json).is_err());
    }

    #[test]
    fn pages_bound_results_skip_invalid_and_unrelated_rows_without_filling_or_retrying() {
        let cfg = Config::default();
        let mut ctx = Context::new(&cfg);
        let mut unrelated = tx();
        unrelated["from"]["hash"] = json!(RECIPIENT);
        let response = raw_response(
            &mut ctx,
            json!({"items":[unrelated,tx(),tx()],"next_page_params":null}),
        );
        let mut p = payload(&qualified(ADDRESS), 2);
        page(&mut p, ADDRESS, 2, &response).unwrap();
        assert_eq!(p.status, "partial");
        assert_eq!(p.data["returned"], 1);
        assert_eq!(p.data["invalid_records"], 1);
        assert_eq!(p.data["provider_page_records"], 3);
        assert_eq!(p.data["truncated"], true);
        assert_eq!(p.data["transactions"][0]["source_pointer"], "/items/1");
        let (p, _, _, _) = run_fixture(
            "evm-address",
            &qualified(ADDRESS),
            json!({"limit":50}),
            1,
            vec![
                (200, json!({"hash":ADDRESS})),
                (200, json!({"items":[tx(),tx()],"next_page_params":null})),
            ],
        );
        assert_eq!(p.data["limit"], 1);
        assert_eq!(p.data["returned"], 1);
        assert_eq!(p.data["truncated"], true);
    }

    #[test]
    fn empty_and_invalid_page_responses_never_claim_full_history() {
        let cfg = Config::default();
        let mut ctx = Context::new(&cfg);
        let mut p = payload(&qualified(ADDRESS), 20);
        page(
            &mut p,
            ADDRESS,
            20,
            &raw_response(&mut ctx, json!({"items":[],"next_page_params":null})),
        )
        .unwrap();
        assert_eq!(p.status, "ok");
        assert_eq!(p.data["returned"], 0);
        assert_eq!(p.data["truncated"], false);
        assert_eq!(p.data["history_complete"], false);
        assert!(p.entities.is_empty());
        for raw in [
            json!({}),
            json!({"items":[]}),
            json!({"items":[],"next_page_params":false}),
            json!({"items":{},"next_page_params":null}),
        ] {
            assert!(page(&mut p, ADDRESS, 20, &raw_response(&mut ctx, raw)).is_err());
        }
    }

    #[test]
    fn provider_denial_retirement_rate_limit_and_schema_failure_stop_without_fallback() {
        for status in [301, 401, 403, 404, 410, 429, 500] {
            let (p, sources, _, urls) = run_fixture(
                "evm-address",
                &qualified(ADDRESS),
                json!({}),
                50,
                vec![(status, json!({"message":"Provider unavailable"}))],
            );
            assert_ne!(p.status, "ok");
            assert_eq!(sources.len(), 1);
            assert_eq!(urls.len(), 1);
            assert!(p.entities.is_empty());
            assert_eq!(p.errors[0]["retryable"], false);
            assert_eq!(p.data["coverage"]["live_availability_verified"], false);
        }
        for bad in [json!({"message":"Key required"}), Value::Null] {
            let (p, _, _, urls) = run_fixture(
                "evm-address",
                &qualified(ADDRESS),
                json!({}),
                50,
                vec![(200, bad)],
            );
            assert_eq!(p.status, "unknown");
            assert_eq!(urls.len(), 1);
        }
        let (p, sources, _, urls) = run_fixture(
            "evm-address",
            &qualified(ADDRESS),
            json!({}),
            50,
            vec![
                (200, json!({"hash":ADDRESS})),
                (403, json!({"message":"Denied"})),
            ],
        );
        assert_eq!(p.status, "partial");
        assert_eq!(p.entities.len(), 1);
        assert_eq!(p.data["returned"], 0);
        assert_eq!(sources.len(), 2);
        assert_eq!(urls.len(), 2);
        assert_eq!(p.errors[0]["code"], "HTTP_BLOCKED");
    }
}
