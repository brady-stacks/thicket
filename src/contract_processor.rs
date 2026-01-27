use crate::cache::Cache;
use clarity::vm::contexts::OwnedEnvironment;
use clarity::vm::costs::analysis::static_cost_from_ast_with_source;
use clarity::vm::costs::ExecutionCost;
use clarity::vm::database::MemoryBackingStore;
use clarity::vm::types::QualifiedContractIdentifier;
use clarity::vm::{ClarityVersion, ast};
use reqwest;
use scraper::{Html, Selector};
use serde_json::json;
use stacks_common::types::StacksEpochId;
use std::sync::Arc;

// example contract urls:
// https://explorer.hiro.so/txid/0x586ed2f2f2f7cfed9d6f1b8812c49a581edc40cf809659d6ad5f8293a3b26b3a?chain=mainnet&tab=sourceCode
// https://explorer.hiro.so/txid/SP3YBY0BH4ANC0Q35QB6PD163F943FVFVDFM1SH7S.gl-api?chain=mainnet&tab=sourceCode
// https://explorer.hiro.so/txid/0x940f5737fdb9de8a916ce3b32cee05c98e9511b807658568edf2d736f1564884?chain=mainnet&tab=sourceCode
pub async fn process_contract_source(
    source_code: &str,
    cache: Arc<Cache>,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    // Step 1: Check cache first (using source code as key)
    let cache_key = source_code;
    let cache_result = tokio::task::spawn_blocking({
        let cache = cache.clone();
        let source = cache_key.to_string();
        move || cache.get(&source)
    })
    .await
    .map_err(|e| format!("Cache lookup error: {}", e))?;

    if let Ok(Some((cached_source_code, cached_cost_map))) = cache_result {
        println!("Cache hit for source code");
        // Ensure block_limits are always present, even in cached responses
        let mut cost_map_with_limits = cached_cost_map.clone();
        ensure_block_limits(&mut cost_map_with_limits);
        return Ok(json!({
            "source_code": cached_source_code,
            "cost_map": cost_map_with_limits
        }));
    }

    println!("Cache miss for source code");

    // Step 2: Clean and process the source code directly (skip HTML parsing)
    let cleaned_source = clean_source_code(source_code);
    let cost_map = analyze_contract_source(&cleaned_source)?;

    // Step 3: Store in cache
    let cache_key_clone = cache_key.to_string();
    let cleaned_source_clone = cleaned_source.clone();
    let cost_map_clone = cost_map.clone();
    tokio::task::spawn_blocking({
        let cache = cache.clone();
        move || {
            if let Err(e) = cache.set(&cache_key_clone, &cleaned_source_clone, &cost_map_clone) {
                eprintln!("Warning: Failed to cache result: {}", e);
            }
        }
    })
    .await
    .ok();

    // Step 4: Return both source code and cost map as JSON
    Ok(json!({
        "source_code": cleaned_source,
        "cost_map": cost_map
    }))
}

pub async fn process_contract_url(
    url: &str,
    cache: Arc<Cache>,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    // Step 1: Check cache first
    let cache_key = url;
    let cache_result = tokio::task::spawn_blocking({
        let cache = cache.clone();
        let url = cache_key.to_string();
        move || cache.get(&url)
    })
    .await
    .map_err(|e| format!("Cache lookup error: {}", e))?;

    if let Ok(Some((cached_source_code, cached_cost_map))) = cache_result {
        println!("Cache hit for URL: {}", url);
        // Ensure block_limits are always present, even in cached responses
        let mut cost_map_with_limits = cached_cost_map.clone();
        ensure_block_limits(&mut cost_map_with_limits);
        return Ok(json!({
            "source_code": cached_source_code,
            "cost_map": cost_map_with_limits
        }));
    }

    println!("Cache miss for URL: {}", url);

    // Step 2: Fetch source code from the URL (already cleaned in fetch_source_code)
    let source_code = fetch_source_code(url).await?;

    // Step 3: Process the source code using static_cost_map
    let cost_map = analyze_contract_source(&source_code)?;

    // Step 4: Store in cache
    let cache_key_clone = cache_key.to_string();
    let source_code_clone = source_code.clone();
    let cost_map_clone = cost_map.clone();
    tokio::task::spawn_blocking({
        let cache = cache.clone();
        move || {
            if let Err(e) = cache.set(&cache_key_clone, &source_code_clone, &cost_map_clone) {
                eprintln!("Warning: Failed to cache result: {}", e);
            }
        }
    })
    .await
    .ok();

    // Step 5: Return both source code and cost map as JSON
    Ok(json!({
        "source_code": source_code,
        "cost_map": cost_map
    }))
}

async fn fetch_source_code(url: &str) -> Result<String, Box<dyn std::error::Error>> {
    // First, try to extract contract identifier from URL and use Stacks API
    if let Ok(source) = try_fetch_from_stacks_api(url).await {
        return Ok(clean_source_code(&source));
    }

    // Fallback to HTML parsing
    let response = reqwest::get(url).await?;
    let html = response.text().await?;

    // Try to find source code in script tags (Hiro Explorer might embed it as JSON)
    if let Some(source) = extract_from_script_tags(&html) {
        return Ok(clean_source_code(&source));
    }

    // Parse HTML and extract source code
    let document = Html::parse_document(&html);

    // Try multiple selectors to find the source code
    let selectors = vec![
        Selector::parse("pre code").unwrap(),
        Selector::parse("code.source-code").unwrap(),
        Selector::parse("pre.source-code").unwrap(),
        Selector::parse("[data-source-code]").unwrap(),
        Selector::parse("code").unwrap(),
        Selector::parse("pre").unwrap(),
    ];

    for selector in selectors {
        for element in document.select(&selector) {
            let text = element.text().collect::<String>();
            let trimmed = text.trim();
            if !trimmed.is_empty()
                && (trimmed.contains("define-public")
                    || trimmed.contains("define-private")
                    || trimmed.contains("define-read-only")
                    || trimmed.contains("define-map")
                    || trimmed.contains("define-constant"))
            {
                return Ok(clean_source_code(&trimmed));
            }
        }
    }

    Err("Could not find source code in the HTML page. The page may load content dynamically via JavaScript.".into())
}

async fn try_fetch_from_stacks_api(url: &str) -> Result<String, Box<dyn std::error::Error>> {
    // Extract contract identifier from Hiro Explorer URL
    // Format: https://explorer.hiro.so/txid/{contract_identifier}?chain=mainnet&tab=sourceCode
    // Or: https://explorer.hiro.so/txid/{address}.{contract_name}?chain=mainnet&tab=sourceCode
    // Or: https://explorer.hiro.so/txid/{txid}?chain=mainnet&tab=sourceCode (transaction URL)

    // First, try to extract contract identifier directly from URL
    let re = regex::Regex::new(r"/([A-Z0-9]{39,41})\.([a-zA-Z0-9_-]+)").unwrap();
    if let Some(caps) = re.captures(url) {
        let address = caps.get(1).unwrap().as_str();
        let contract_name = caps.get(2).unwrap().as_str();

        // Determine chain from URL
        let chain = if url.contains("chain=mainnet") {
            "mainnet"
        } else {
            "testnet"
        };
        let api_base = if chain == "mainnet" {
            "https://api.hiro.so"
        } else {
            "https://api.testnet.hiro.so"
        };

        let api_url = format!(
            "{}/v2/contracts/source/{}/{}",
            api_base, address, contract_name
        );
        let response = reqwest::get(&api_url).await?;

        if response.status().is_success() {
            let json: serde_json::Value = response.json().await?;
            if let Some(source) = json.get("source").and_then(|s| s.as_str()) {
                return Ok(source.to_string());
            }
        }
    }

    // If URL is a transaction URL, try to fetch transaction and extract contract identifier
    // Format: https://explorer.hiro.so/txid/{txid}?chain=mainnet&tab=sourceCode
    let txid_re = regex::Regex::new(r"/txid/(0x[a-fA-F0-9]+)").unwrap();
    if let Some(caps) = txid_re.captures(url) {
        let txid = caps.get(1).unwrap().as_str();

        // Determine chain from URL
        let chain = if url.contains("chain=mainnet") {
            "mainnet"
        } else {
            "testnet"
        };
        let api_base = if chain == "mainnet" {
            "https://api.hiro.so"
        } else {
            "https://api.testnet.hiro.so"
        };

        // Fetch transaction details
        let tx_url = format!("{}/extended/v1/tx/{}", api_base, txid);
        let response = reqwest::get(&tx_url).await?;

        if response.status().is_success() {
            let json: serde_json::Value = response.json().await?;

            // Try to extract contract identifier from transaction
            if let Some(contract_call) = json.get("contract_call") {
                if let Some(contract_id) = contract_call.get("contract_id") {
                    if let Some(contract_id_str) = contract_id.as_str() {
                        // Format: SP3Q...9HYGY.options-vault-v13
                        if let Some(dot_pos) = contract_id_str.find('.') {
                            let address = &contract_id_str[..dot_pos];
                            let contract_name = &contract_id_str[dot_pos + 1..];

                            let source_url = format!(
                                "{}/v2/contracts/source/{}/{}",
                                api_base, address, contract_name
                            );
                            let source_response = reqwest::get(&source_url).await?;

                            if source_response.status().is_success() {
                                let source_json: serde_json::Value = source_response.json().await?;
                                if let Some(source) =
                                    source_json.get("source").and_then(|s| s.as_str())
                                {
                                    return Ok(source.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Err("Could not fetch from Stacks API".into())
}

fn extract_from_script_tags(html: &str) -> Option<String> {
    // Look for JSON data in script tags that might contain the source code
    let re = regex::Regex::new(r#"(?s)<script[^>]*>(.*?)</script>"#).unwrap();

    for cap in re.captures_iter(html) {
        let script_content = cap.get(1).unwrap().as_str();

        // Try to parse as JSON and look for source code
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(script_content) {
            if let Some(source) = find_source_in_json(&json) {
                return Some(source);
            }
        }

        // Also check if the script content itself looks like Clarity code
        if script_content.contains("define-public") || script_content.contains("define-private") {
            // Extract just the Clarity code part
            if let Some(start) = script_content.find("define-public") {
                if let Some(end) = script_content[start..].rfind(")") {
                    let code = &script_content[start..start + end + 1];
                    if code.len() > 50 {
                        // Reasonable minimum length
                        return Some(code.to_string());
                    }
                }
            }
        }
    }

    None
}

fn find_source_in_json(json: &serde_json::Value) -> Option<String> {
    match json {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if key == "source" || key == "sourceCode" || key == "source_code" {
                    if let Some(s) = value.as_str() {
                        return Some(s.to_string());
                    }
                }
                if let Some(s) = find_source_in_json(value) {
                    return Some(s);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Some(s) = find_source_in_json(item) {
                    return Some(s);
                }
            }
        }
        _ => {}
    }
    None
}

/// Clean source code by handling escape sequences and removing problematic characters
fn clean_source_code(source: &str) -> String {
    // If the source looks like it might be a JSON-encoded string (starts/ends with quotes),
    // try to parse it as JSON to unescape properly
    let trimmed = source.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') {
        // Try to parse as JSON string to handle escape sequences
        if let Ok(parsed) = serde_json::from_str::<String>(trimmed) {
            return parsed;
        }
    }

    // Otherwise, handle common escape sequences manually
    // Replace common escape sequences that might cause issues
    let mut cleaned = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            // Handle escape sequences
            match chars.peek() {
                Some('n') => {
                    chars.next();
                    cleaned.push('\n');
                }
                Some('t') => {
                    chars.next();
                    cleaned.push('\t');
                }
                Some('r') => {
                    chars.next();
                    cleaned.push('\r');
                }
                Some('\\') => {
                    chars.next();
                    cleaned.push('\\');
                }
                Some('"') => {
                    chars.next();
                    cleaned.push('"');
                }
                Some('\'') => {
                    chars.next();
                    cleaned.push('\'');
                }
                Some('u') => {
                    // Handle \uXXXX unicode escapes - try to parse 4 hex digits
                    chars.next(); // consume 'u'
                    let mut hex_chars = String::new();
                    for _ in 0..4 {
                        if let Some(&next_ch) = chars.peek() {
                            if next_ch.is_ascii_hexdigit() {
                                hex_chars.push(chars.next().unwrap());
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    if hex_chars.len() == 4 {
                        if let Ok(code_point) = u32::from_str_radix(&hex_chars, 16) {
                            if let Some(unicode_char) = char::from_u32(code_point) {
                                cleaned.push(unicode_char);
                            } else {
                                // Invalid unicode, skip the escape sequence
                            }
                        } else {
                            // Invalid hex, skip the escape sequence
                        }
                    } else {
                        // Not enough hex digits, skip the escape sequence
                    }
                }
                _ => {
                    // Unknown escape sequence - remove the backslash to avoid parser errors
                    // This handles cases where there's a stray backslash that Clarity doesn't support
                    // The next character will be processed normally
                }
            }
        } else {
            cleaned.push(ch);
        }
    }

    cleaned
}

fn get_block_limits() -> ExecutionCost {
    // Block limits for mainnet Epoch33
    ExecutionCost {
        runtime: 5_000_000_000,      // 5 billion
        read_count: 7_300,            // 7,300
        read_length: 100_000_000,     // 100 million
        write_count: 7_300,           // 7,300
        write_length: 15_000_000,     // 15 million
    }
}

fn ensure_block_limits(cost_map: &mut serde_json::Value) {
    if !cost_map.get("block_limits").is_some() {
        let block_limit = get_block_limits();
        cost_map["block_limits"] = json!({
            "runtime": block_limit.runtime,
            "read_count": block_limit.read_count,
            "read_length": block_limit.read_length,
            "write_count": block_limit.write_count,
            "write_length": block_limit.write_length,
        });
    }
}

fn analyze_contract_source(source: &str) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let contract_id = QualifiedContractIdentifier::transient();
    let epoch = StacksEpochId::Epoch33; // Using latest epoch
    let clarity_version = ClarityVersion::Clarity4; // Using latest clarity version

    // Build AST from source code
    let ast = ast::build_ast(&contract_id, source, &mut (), clarity_version, epoch)
        .map_err(|e| format!("Failed to build AST: {:?}", e))?;

    // Set up environment similar to run_cost_analysis_test
    let mut memory_store = MemoryBackingStore::new();
    let db = memory_store.as_clarity_db();
    // Use new_free for a free cost tracker (suitable for static analysis)
    let mut owned_env =
        OwnedEnvironment::new_free(false, stacks_common::consts::CHAIN_ID_TESTNET, db, epoch);

    // Get static cost map (not tree)
    let static_cost_map = owned_env
        .with_cost_analysis_environment(&contract_id, clarity_version, |env| {
            static_cost_from_ast_with_source(&ast, &clarity_version, epoch, Some(source), env)
        })
        .map_err(|e| format!("Failed to get static cost map: {}", e))?;

    // Get block limit for mainnet
    let block_limit = get_block_limits();

    // Convert cost map to JSON-serializable format
    let mut result = json!({});

    for (function_name, (static_cost, trait_count)) in static_cost_map {
        result[function_name] = json!({
            "cost": {
                "min": {
                    "runtime": static_cost.min.runtime,
                    "read_count": static_cost.min.read_count,
                    "read_length": static_cost.min.read_length,
                    "write_count": static_cost.min.write_count,
                    "write_length": static_cost.min.write_length,
                },
                "max": {
                    "runtime": static_cost.max.runtime,
                    "read_count": static_cost.max.read_count,
                    "read_length": static_cost.max.read_length,
                    "write_count": static_cost.max.write_count,
                    "write_length": static_cost.max.write_length,
                }
            },
            "trait_count": trait_count,
        });
    }

    // Add block limits to the result
    result["block_limits"] = json!({
        "runtime": block_limit.runtime,
        "read_count": block_limit.read_count,
        "read_length": block_limit.read_length,
        "write_count": block_limit.write_count,
        "write_length": block_limit.write_length,
    });

    Ok(result)
}
