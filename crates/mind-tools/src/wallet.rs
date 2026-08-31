//! WALLET INTENTS — a fail-closed boundary in front of any future signer integration.
//!
//! This module cannot sign or broadcast. It contains no seed phrase, private key, RPC endpoint, or
//! calldata field. Its only job is to decide whether a fully priced swap intent is inside an
//! operator-authored envelope before a separate approval/signer layer is even allowed to see it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WalletSwapIntent {
    /// Canonical account id (prefer CAIP-10), matched exactly against the policy.
    pub account: String,
    /// Canonical chain id (prefer CAIP-2), matched exactly against the policy.
    pub chain: String,
    /// Canonical asset ids (prefer CAIP-19). The pricing adapter, not the model, supplies USD data.
    pub sell_asset: String,
    pub buy_asset: String,
    /// Canonical router/contract account id. Arbitrary calldata is deliberately not represented.
    pub router: String,
    /// Trusted-oracle notional, after quote construction.
    pub notional_usd: f64,
    pub slippage_bps: u16,
    pub estimated_network_fee_usd: f64,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
}

/// Empty allowlists deny everything. There is intentionally no permissive default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WalletExecutionPolicy {
    pub allowed_accounts: Vec<String>,
    pub allowed_chains: Vec<String>,
    pub allowed_assets: Vec<String>,
    pub allowed_routers: Vec<String>,
    pub max_notional_usd: f64,
    pub max_slippage_bps: u16,
    pub max_network_fee_usd: f64,
    pub max_intent_ttl_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalletIntentRefusal {
    AccountNotAllowed,
    ChainNotAllowed,
    AssetNotAllowed,
    SameAsset,
    RouterNotAllowed,
    InvalidNotional,
    NotionalTooLarge,
    SlippageTooHigh,
    InvalidNetworkFee,
    NetworkFeeTooHigh,
    FutureDated,
    Expired,
    InvalidLifetime,
    LifetimeTooLong,
}

fn allowed(value: &str, allowlist: &[String]) -> bool {
    !value.trim().is_empty() && allowlist.iter().any(|allowed| allowed == value)
}

/// Validate one immutable intent. A success is permission to request approval—not permission to
/// sign or broadcast. The signer remains a separate capability with its own human/policy gate.
pub fn validate_wallet_swap(
    intent: &WalletSwapIntent,
    policy: &WalletExecutionPolicy,
    now_ms: i64,
) -> Result<(), WalletIntentRefusal> {
    if !allowed(&intent.account, &policy.allowed_accounts) {
        return Err(WalletIntentRefusal::AccountNotAllowed);
    }
    if !allowed(&intent.chain, &policy.allowed_chains) {
        return Err(WalletIntentRefusal::ChainNotAllowed);
    }
    if !allowed(&intent.sell_asset, &policy.allowed_assets)
        || !allowed(&intent.buy_asset, &policy.allowed_assets)
    {
        return Err(WalletIntentRefusal::AssetNotAllowed);
    }
    if intent.sell_asset == intent.buy_asset {
        return Err(WalletIntentRefusal::SameAsset);
    }
    if !allowed(&intent.router, &policy.allowed_routers) {
        return Err(WalletIntentRefusal::RouterNotAllowed);
    }
    if !intent.notional_usd.is_finite() || intent.notional_usd <= 0.0 {
        return Err(WalletIntentRefusal::InvalidNotional);
    }
    if !policy.max_notional_usd.is_finite()
        || policy.max_notional_usd <= 0.0
        || intent.notional_usd > policy.max_notional_usd
    {
        return Err(WalletIntentRefusal::NotionalTooLarge);
    }
    if intent.slippage_bps > policy.max_slippage_bps {
        return Err(WalletIntentRefusal::SlippageTooHigh);
    }
    if !intent.estimated_network_fee_usd.is_finite() || intent.estimated_network_fee_usd < 0.0 {
        return Err(WalletIntentRefusal::InvalidNetworkFee);
    }
    if !policy.max_network_fee_usd.is_finite()
        || policy.max_network_fee_usd < 0.0
        || intent.estimated_network_fee_usd > policy.max_network_fee_usd
    {
        return Err(WalletIntentRefusal::NetworkFeeTooHigh);
    }
    if intent.created_at_ms > now_ms {
        return Err(WalletIntentRefusal::FutureDated);
    }
    if intent.expires_at_ms <= now_ms {
        return Err(WalletIntentRefusal::Expired);
    }
    let lifetime = intent.expires_at_ms.saturating_sub(intent.created_at_ms);
    if lifetime <= 0 || policy.max_intent_ttl_ms <= 0 {
        return Err(WalletIntentRefusal::InvalidLifetime);
    }
    if lifetime > policy.max_intent_ttl_ms {
        return Err(WalletIntentRefusal::LifetimeTooLong);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> WalletExecutionPolicy {
        WalletExecutionPolicy {
            allowed_accounts: vec!["eip155:8453:0xaccount".into()],
            allowed_chains: vec!["eip155:8453".into()],
            allowed_assets: vec!["base:USDC".into(), "base:WETH".into()],
            allowed_routers: vec!["eip155:8453:0xrouter".into()],
            max_notional_usd: 100.0,
            max_slippage_bps: 50,
            max_network_fee_usd: 2.0,
            max_intent_ttl_ms: 60_000,
        }
    }

    fn intent() -> WalletSwapIntent {
        WalletSwapIntent {
            account: "eip155:8453:0xaccount".into(),
            chain: "eip155:8453".into(),
            sell_asset: "base:USDC".into(),
            buy_asset: "base:WETH".into(),
            router: "eip155:8453:0xrouter".into(),
            notional_usd: 50.0,
            slippage_bps: 30,
            estimated_network_fee_usd: 0.20,
            created_at_ms: 1_000,
            expires_at_ms: 31_000,
        }
    }

    #[test]
    fn an_empty_policy_denies_every_wallet() {
        assert_eq!(
            validate_wallet_swap(&intent(), &WalletExecutionPolicy::default(), 2_000),
            Err(WalletIntentRefusal::AccountNotAllowed)
        );
    }

    #[test]
    fn a_fully_allowlisted_short_lived_intent_may_request_approval() {
        assert_eq!(validate_wallet_swap(&intent(), &policy(), 2_000), Ok(()));
    }

    #[test]
    fn value_slippage_fee_and_expiry_caps_fail_closed() {
        let mut candidate = intent();
        candidate.notional_usd = 100.01;
        assert_eq!(
            validate_wallet_swap(&candidate, &policy(), 2_000),
            Err(WalletIntentRefusal::NotionalTooLarge)
        );
        candidate = intent();
        candidate.slippage_bps = 51;
        assert_eq!(
            validate_wallet_swap(&candidate, &policy(), 2_000),
            Err(WalletIntentRefusal::SlippageTooHigh)
        );
        candidate = intent();
        candidate.estimated_network_fee_usd = 2.01;
        assert_eq!(
            validate_wallet_swap(&candidate, &policy(), 2_000),
            Err(WalletIntentRefusal::NetworkFeeTooHigh)
        );
        candidate = intent();
        candidate.expires_at_ms = 2_000;
        assert_eq!(
            validate_wallet_swap(&candidate, &policy(), 2_000),
            Err(WalletIntentRefusal::Expired)
        );
    }

    #[test]
    fn an_unknown_router_or_asset_never_reaches_a_signer() {
        let mut candidate = intent();
        candidate.router = "eip155:8453:0xmalicious".into();
        assert_eq!(
            validate_wallet_swap(&candidate, &policy(), 2_000),
            Err(WalletIntentRefusal::RouterNotAllowed)
        );
        candidate = intent();
        candidate.buy_asset = "base:SCAM".into();
        assert_eq!(
            validate_wallet_swap(&candidate, &policy(), 2_000),
            Err(WalletIntentRefusal::AssetNotAllowed)
        );
    }
}
