use clarity::vm::contexts::OwnedEnvironment;
use clarity::vm::costs::analysis::static_cost_from_ast_with_source;
use clarity::vm::database::MemoryBackingStore;
use clarity::vm::types::QualifiedContractIdentifier;
use clarity::vm::{ClarityVersion, ast};
use reqwest;
use scraper::{Html, Selector};
use serde_json::json;
use stacks_common::types::StacksEpochId;

pub async fn process_contract_url(url: &str) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    // Step 2: Fetch source code from the URL
    let source_code = fetch_source_code(url).await?;
    
    // Step 3: Process the source code using static_cost_map
    let cost_map = analyze_contract_source(&source_code)?;
    
    // Step 4: Return both source code and cost map as JSON
    Ok(json!({
        "source_code": source_code,
        "cost_map": cost_map
    }))
}

async fn fetch_source_code(url: &str) -> Result<String, Box<dyn std::error::Error>> {
    // First, try to extract contract identifier from URL and use Stacks API
    if let Ok(source) = try_fetch_from_stacks_api(url).await {
        return Ok(source);
    }
    
    // Fallback to HTML parsing
    let response = reqwest::get(url).await?;
    let html = response.text().await?;
    
    // Try to find source code in script tags (Hiro Explorer might embed it as JSON)
    if let Some(source) = extract_from_script_tags(&html) {
        return Ok(source);
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
                    || trimmed.contains("define-constant")) {
                return Ok(trimmed.to_string());
            }
        }
    }
    
    Err("Could not find source code in the HTML page. The page may load content dynamically via JavaScript.".into())
}

async fn try_fetch_from_stacks_api(url: &str) -> Result<String, Box<dyn std::error::Error>> {
    // Extract contract identifier from Hiro Explorer URL
    // Format: https://explorer.hiro.so/txid/{contract_identifier}?chain=mainnet&tab=sourceCode
    // Or: https://explorer.hiro.so/txid/{address}.{contract_name}?chain=mainnet&tab=sourceCode
    
    let re = regex::Regex::new(r"/([A-Z0-9]{39,41})\.([a-zA-Z0-9_-]+)").unwrap();
    if let Some(caps) = re.captures(url) {
        let address = caps.get(1).unwrap().as_str();
        let contract_name = caps.get(2).unwrap().as_str();
        
        // Determine chain from URL
        let chain = if url.contains("chain=mainnet") { "mainnet" } else { "testnet" };
        let api_base = if chain == "mainnet" {
            "https://api.hiro.so"
        } else {
            "https://api.testnet.hiro.so"
        };
        
        let api_url = format!("{}/v2/contracts/source/{}/{}", api_base, address, contract_name);
        let response = reqwest::get(&api_url).await?;
        
        if response.status().is_success() {
            let json: serde_json::Value = response.json().await?;
            if let Some(source) = json.get("source").and_then(|s| s.as_str()) {
                return Ok(source.to_string());
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
                    let code = &script_content[start..start+end+1];
                    if code.len() > 50 { // Reasonable minimum length
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
    let mut owned_env = OwnedEnvironment::new_free(false, stacks_common::consts::CHAIN_ID_TESTNET, db, epoch);
    
    // Get static cost map (not tree)
    let static_cost_map = owned_env
        .with_cost_analysis_environment(&contract_id, clarity_version, |env| {
            static_cost_from_ast_with_source(
                &ast,
                &clarity_version,
                epoch,
                Some(source),
                env,
            )
        })
        .map_err(|e| format!("Failed to get static cost map: {}", e))?;
    
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
    
    Ok(result)
}
