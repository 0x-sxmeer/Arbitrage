# fund_executors.ps1
# Script to supply native gas tokens to executor contracts for transaction fees
param (
    [string]$ArbitrumContract = "0xYourArbitrumContractAddress",
    [string]$SolanaContract = "YourSolanaContractAddress"
)

Write-Host "Capital Initialization for Arbitrage Executors"
Write-Host "=============================================="

# 1. Fund EVM Contract (Arbitrum)
# Ensure you have cast (Foundry) installed and the PRIVATE_KEY env var set
if ($env:PRIVATE_KEY) {
    Write-Host "Funding Arbitrum Executor with 0.05 ETH..."
    cast send $ArbitrumContract --value 0.05ether --private-key $env:PRIVATE_KEY --rpc-url "https://arb1.arbitrum.io/rpc"
} else {
    Write-Host "[WARNING] PRIVATE_KEY environment variable not set. Skipping EVM funding."
}

# 2. Fund SVM Contract (Solana)
# Ensure you have Solana CLI installed
try {
    Write-Host "Funding Solana Executor with 0.5 SOL on Devnet..."
    solana airdrop 0.5 $SolanaContract --url devnet
} catch {
    Write-Host "[WARNING] solana-cli not found or airdrop failed. Skipping SVM funding."
}

Write-Host "Capital Initialization Complete."
