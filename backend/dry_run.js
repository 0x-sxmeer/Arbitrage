const { ethers } = require("ethers");
require("dotenv").config();

async function main() {
  console.log("🚀 Initializing Dry-Run Execution Test on Base...");
  
  const provider = new ethers.JsonRpcProvider(process.env.BASE_HTTP_URL);
  const wallet = new ethers.Wallet(process.env.PRIVATE_KEY, provider);
  
  const contractAddress = process.env.CONTRACT_ADDRESS;
  console.log("✅ Target Contract:", contractAddress);
  console.log("✅ Signer Wallet:", wallet.address);

  // Minimal ABI for the AtomicArb execution
  const abi = [
    "function executeArb(address asset, uint256 flashAmount, bytes[] calldata routePayloads, uint256 minProfit) external"
  ];
  const contract = new ethers.Contract(contractAddress, abi, wallet);

  // Dummy payload for WETH
  const WETH = "0x4200000000000000000000000000000000000006";
  const flashAmount = ethers.parseEther("0.1"); // 0.1 WETH

  console.log("\n📡 Sending dry-run (eth_call) to executeArb...");
  try {
    // We expect this to revert with "Not enough profit" or similar because it's a dummy payload
    // BUT if it reverts with that, it means the pipeline and contract are fully operational!
    const result = await contract.executeArb.staticCall(
      WETH,
      flashAmount,
      [], // Empty route
      0,  // minProfit
    );
    console.log("🎉 Execution succeeded! Result:", result);
  } catch (error) {
    console.log("⚠️ Dry-run Reverted (Expected for dummy payload)");
    // The exact error depends on the contract's require statements
    if (error.reason) {
      console.log("Reason:", error.reason);
    } else {
      console.log("Error:", error.message.substring(0, 200) + "...");
    }
  }
}

main().catch(console.error);
