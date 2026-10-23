//! Explicit, serial probe of the preview's international activity adapter.
//! No credentials are printed. Without --run this only reads local status.
use buddy_switch_core::modules::{account, buddy_activity, candidate, region::Region};
use serde_json::json;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let index = args
        .iter()
        .position(|arg| arg == "--account-index")
        .and_then(|i| args.get(i + 1))
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|i| *i > 0)
        .expect("--account-index must be a positive 1-based index");
    candidate::initialize().expect("preview data root");
    let accounts = account::load_accounts_for(Region::Global);
    let account = accounts.get(index - 1).expect("account index out of range");
    let value = if args.iter().any(|arg| arg == "--run") {
        buddy_activity::run(account).await
    } else {
        buddy_activity::status(account)
    };
    println!(
        "{}",
        json!({"accountIndex": index, "result": value["result"],
        "status": value["status"], "error": value["error"],
        "evidence": value["evidence"], "amount": value["amount"]})
    );
}
