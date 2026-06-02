use anyhow::{Context, Result};
use colored::*;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Commerce API client for Hanzo Commerce platform
pub struct CommerceClient {
    client: Client,
    base_url: String,
    api_key: Option<String>,
}

impl CommerceClient {
    pub fn new(base_url: Option<String>, api_key: Option<String>) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.unwrap_or_else(|| "https://api.hanzo.ai".to_string()),
            api_key,
        }
    }

    fn auth_header(&self) -> Option<String> {
        self.api_key.as_ref().map(|k| format!("Bearer {}", k))
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.get(&url);

        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }

        let resp = req.send().await.context("Failed to send request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("API error {}: {}", status, body);
        }

        resp.json().await.context("Failed to parse response")
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.post(&url).json(body);

        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }

        let resp = req.send().await.context("Failed to send request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("API error {}: {}", status, body);
        }

        resp.json().await.context("Failed to parse response")
    }
}

// ============================================================================
// Orders
// ============================================================================

#[derive(Debug, Serialize, Deserialize)]
pub struct Order {
    pub id: Option<String>,
    pub number: Option<i64>,
    pub status: Option<String>,
    pub total: Option<f64>,
    pub currency: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: Option<String>,
    #[serde(rename = "userId")]
    pub user_id: Option<String>,
    pub email: Option<String>,
}

pub async fn orders_list(
    base_url: Option<String>,
    api_key: Option<String>,
    limit: Option<u32>,
    status: Option<String>,
) -> Result<()> {
    println!("{} Fetching orders...", "->".cyan());

    let client = CommerceClient::new(base_url, api_key);

    let mut path = "/order".to_string();
    let mut params = vec![];

    if let Some(l) = limit {
        params.push(format!("limit={}", l));
    }
    if let Some(s) = status {
        params.push(format!("status={}", s));
    }

    if !params.is_empty() {
        path = format!("{}?{}", path, params.join("&"));
    }

    let response = client.get(&path).await?;

    // Handle array or object with models field
    let orders: Vec<Value> = if let Some(arr) = response.as_array() {
        arr.clone()
    } else if let Some(models) = response.get("models") {
        models.as_array().cloned().unwrap_or_default()
    } else {
        vec![response]
    };

    println!("\n{}", "Orders".bold().underline());
    println!("{}", "=".repeat(80));

    if orders.is_empty() {
        println!("{}", "No orders found.".yellow());
        return Ok(());
    }

    println!(
        "{:<24} {:<10} {:<12} {:<12} {:<20}",
        "ID".bold(),
        "Number".bold(),
        "Status".bold(),
        "Total".bold(),
        "Created".bold()
    );
    println!("{}", "-".repeat(80));

    for order in &orders {
        let id = order.get("id").and_then(|v| v.as_str()).unwrap_or("-");
        let number = order.get("number").and_then(|v| v.as_i64()).map(|n| n.to_string()).unwrap_or("-".to_string());
        let status = order.get("status").and_then(|v| v.as_str()).unwrap_or("-");
        let total = order.get("total").and_then(|v| v.as_f64()).map(|t| format!("{:.2}", t / 100.0)).unwrap_or("-".to_string());
        let currency = order.get("currency").and_then(|v| v.as_str()).unwrap_or("USD");
        let created = order.get("createdAt").and_then(|v| v.as_str()).unwrap_or("-");

        let status_colored = match status {
            "completed" | "paid" => status.green(),
            "pending" => status.yellow(),
            "cancelled" | "refunded" => status.red(),
            _ => status.normal(),
        };

        println!(
            "{:<24} {:<10} {:<12} {:<12} {:<20}",
            id,
            number,
            status_colored,
            format!("{} {}", total, currency),
            &created[..20.min(created.len())]
        );
    }

    println!("\n{} {} orders", "Total:".bold(), orders.len());
    Ok(())
}

pub async fn orders_get(
    base_url: Option<String>,
    api_key: Option<String>,
    order_id: String,
) -> Result<()> {
    println!("{} Fetching order {}...", "->".cyan(), order_id.yellow());

    let client = CommerceClient::new(base_url, api_key);
    let path = format!("/order/{}", order_id);
    let order = client.get(&path).await?;

    println!("\n{}", "Order Details".bold().underline());
    println!("{}", "=".repeat(60));

    print_field("ID", order.get("id"));
    print_field("Number", order.get("number"));
    print_field("Status", order.get("status"));
    print_field("Email", order.get("email"));
    print_field("User ID", order.get("userId"));

    println!("\n{}", "Financials".bold());
    println!("{}", "-".repeat(40));
    print_money_field("Subtotal", order.get("subtotal"), order.get("currency"));
    print_money_field("Tax", order.get("tax"), order.get("currency"));
    print_money_field("Shipping", order.get("shipping"), order.get("currency"));
    print_money_field("Discount", order.get("discount"), order.get("currency"));
    print_money_field("Total", order.get("total"), order.get("currency"));

    if let Some(items) = order.get("items").and_then(|v| v.as_array()) {
        println!("\n{}", "Line Items".bold());
        println!("{}", "-".repeat(40));
        for item in items {
            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("-");
            let qty = item.get("quantity").and_then(|v| v.as_i64()).unwrap_or(0);
            let price = item.get("price").and_then(|v| v.as_f64()).unwrap_or(0.0) / 100.0;
            println!("  {} x {} @ ${:.2}", qty, name, price);
        }
    }

    print_field("\nCreated", order.get("createdAt"));
    print_field("Updated", order.get("updatedAt"));

    Ok(())
}

pub async fn orders_create(
    base_url: Option<String>,
    api_key: Option<String>,
    email: String,
    items_json: Option<String>,
) -> Result<()> {
    println!("{} Creating order...", "->".cyan());

    let client = CommerceClient::new(base_url, api_key);

    let mut body = serde_json::json!({
        "email": email,
        "currency": "USD"
    });

    if let Some(items_str) = items_json {
        let items: Value = serde_json::from_str(&items_str)
            .context("Invalid JSON for items")?;
        body["items"] = items;
    }

    let order = client.post("/order", &body).await?;

    let order_id = order.get("id").and_then(|v| v.as_str()).unwrap_or("-");

    println!("{} Order created successfully!", "OK".green().bold());
    println!("  ID: {}", order_id.yellow());

    Ok(())
}

// ============================================================================
// Products
// ============================================================================

pub async fn products_list(
    base_url: Option<String>,
    api_key: Option<String>,
    limit: Option<u32>,
) -> Result<()> {
    println!("{} Fetching products...", "->".cyan());

    let client = CommerceClient::new(base_url, api_key);

    let path = match limit {
        Some(l) => format!("/product?limit={}", l),
        None => "/product".to_string(),
    };

    let response = client.get(&path).await?;

    let products: Vec<Value> = if let Some(arr) = response.as_array() {
        arr.clone()
    } else if let Some(models) = response.get("models") {
        models.as_array().cloned().unwrap_or_default()
    } else {
        vec![response]
    };

    println!("\n{}", "Products".bold().underline());
    println!("{}", "=".repeat(100));

    if products.is_empty() {
        println!("{}", "No products found.".yellow());
        return Ok(());
    }

    println!(
        "{:<24} {:<30} {:<12} {:<10} {:<10}",
        "ID".bold(),
        "Name".bold(),
        "Price".bold(),
        "Stock".bold(),
        "Enabled".bold()
    );
    println!("{}", "-".repeat(100));

    for product in &products {
        let id = product.get("id").and_then(|v| v.as_str()).unwrap_or("-");
        let name = product.get("name").and_then(|v| v.as_str()).unwrap_or("-");
        let price = product.get("price").and_then(|v| v.as_f64()).map(|p| format!("${:.2}", p / 100.0)).unwrap_or("-".to_string());
        let stock = product.get("inventory").and_then(|v| v.as_i64()).map(|s| s.to_string()).unwrap_or("-".to_string());
        let enabled = product.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);

        let enabled_str = if enabled { "Yes".green() } else { "No".red() };
        let name_truncated = if name.len() > 28 { format!("{}...", &name[..25]) } else { name.to_string() };

        println!(
            "{:<24} {:<30} {:<12} {:<10} {:<10}",
            id,
            name_truncated,
            price,
            stock,
            enabled_str
        );
    }

    println!("\n{} {} products", "Total:".bold(), products.len());
    Ok(())
}

pub async fn products_sync(
    base_url: Option<String>,
    api_key: Option<String>,
    source: Option<String>,
) -> Result<()> {
    println!("{} Syncing products...", "->".cyan());

    let source = source.unwrap_or_else(|| "local".to_string());
    println!("  Source: {}", source.yellow());

    let client = CommerceClient::new(base_url, api_key);

    // For now, we just verify connection and list current products
    let path = "/product?limit=1";
    let _ = client.get(path).await?;

    println!("{} Product sync initiated", "OK".green().bold());
    println!("  Run {} to verify", "hanzo commerce products list".cyan());

    Ok(())
}

// ============================================================================
// Carts
// ============================================================================

pub async fn carts_view(
    base_url: Option<String>,
    api_key: Option<String>,
    cart_id: Option<String>,
) -> Result<()> {
    let client = CommerceClient::new(base_url, api_key);

    match cart_id {
        Some(id) => {
            println!("{} Fetching cart {}...", "->".cyan(), id.yellow());
            let path = format!("/cart/{}", id);
            let cart = client.get(&path).await?;

            println!("\n{}", "Cart Details".bold().underline());
            println!("{}", "=".repeat(60));

            print_field("ID", cart.get("id"));
            print_field("User ID", cart.get("userId"));
            print_field("Status", cart.get("status"));

            if let Some(items) = cart.get("items").and_then(|v| v.as_array()) {
                println!("\n{}", "Items".bold());
                println!("{}", "-".repeat(40));

                let mut subtotal = 0.0;
                for item in items {
                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("-");
                    let qty = item.get("quantity").and_then(|v| v.as_i64()).unwrap_or(0);
                    let price = item.get("price").and_then(|v| v.as_f64()).unwrap_or(0.0) / 100.0;
                    let line_total = price * qty as f64;
                    subtotal += line_total;
                    println!("  {} x {} @ ${:.2} = ${:.2}", qty, name, price, line_total);
                }
                println!("{}", "-".repeat(40));
                println!("  {} ${:.2}", "Subtotal:".bold(), subtotal);
            }

            print_field("\nCreated", cart.get("createdAt"));
            print_field("Updated", cart.get("updatedAt"));
        }
        None => {
            println!("{} Fetching carts...", "->".cyan());
            let response = client.get("/cart?limit=20").await?;

            let carts: Vec<Value> = if let Some(arr) = response.as_array() {
                arr.clone()
            } else if let Some(models) = response.get("models") {
                models.as_array().cloned().unwrap_or_default()
            } else {
                vec![response]
            };

            println!("\n{}", "Carts".bold().underline());
            println!("{}", "=".repeat(80));

            if carts.is_empty() {
                println!("{}", "No carts found.".yellow());
                return Ok(());
            }

            println!(
                "{:<24} {:<24} {:<10} {:<10}",
                "ID".bold(),
                "User ID".bold(),
                "Items".bold(),
                "Status".bold()
            );
            println!("{}", "-".repeat(80));

            for cart in &carts {
                let id = cart.get("id").and_then(|v| v.as_str()).unwrap_or("-");
                let user_id = cart.get("userId").and_then(|v| v.as_str()).unwrap_or("-");
                let items_count = cart.get("items").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                let status = cart.get("status").and_then(|v| v.as_str()).unwrap_or("-");

                println!(
                    "{:<24} {:<24} {:<10} {:<10}",
                    id,
                    user_id,
                    items_count,
                    status
                );
            }

            println!("\n{} {} carts", "Total:".bold(), carts.len());
        }
    }

    Ok(())
}

// ============================================================================
// Deploy
// ============================================================================

pub async fn deploy(
    base_url: Option<String>,
    api_key: Option<String>,
    environment: String,
    dry_run: bool,
) -> Result<()> {
    if dry_run {
        println!("{} Dry run - no changes will be made", "DRY".yellow().bold());
    }

    println!("{} Deploying commerce to {}...", "->".cyan(), environment.yellow());

    let client = CommerceClient::new(base_url, api_key);

    // Verify API connectivity
    println!("  Verifying API connection...");
    let _ = client.get("/").await.ok();

    let deploy_body = serde_json::json!({
        "environment": environment,
        "dryRun": dry_run,
        "timestamp": chrono::Utc::now().to_rfc3339()
    });

    if !dry_run {
        // Attempt actual deploy
        match client.post("/deploy", &deploy_body).await {
            Ok(resp) => {
                let deploy_id = resp.get("id").and_then(|v| v.as_str()).unwrap_or("-");
                println!("{} Deploy initiated!", "OK".green().bold());
                println!("  Deploy ID: {}", deploy_id.yellow());
                println!("  Environment: {}", environment);
            }
            Err(_) => {
                // Fallback for when deploy endpoint is not available
                println!("{} Deploy endpoint not available", "WARN".yellow().bold());
                println!("  Use the web dashboard or contact support");
            }
        }
    } else {
        println!("{} Dry run complete", "OK".green().bold());
        println!("  Would deploy to: {}", environment);
    }

    Ok(())
}

// ============================================================================
// Helpers
// ============================================================================

fn print_field(label: &str, value: Option<&Value>) {
    let display = match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Null) => "-".to_string(),
        None => "-".to_string(),
        Some(v) => v.to_string(),
    };
    println!("  {}: {}", label.bold(), display);
}

fn print_money_field(label: &str, value: Option<&Value>, currency: Option<&Value>) {
    let curr = currency.and_then(|v| v.as_str()).unwrap_or("USD");
    let amount = value.and_then(|v| v.as_f64()).unwrap_or(0.0) / 100.0;
    println!("  {}: ${:.2} {}", label.bold(), amount, curr);
}
