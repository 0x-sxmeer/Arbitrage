use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke,
};

// Replace this with your actual program ID when deployed
declare_id!("ArbEngine1111111111111111111111111111111111");

#[program]
pub mod solana_atomic_arb {
    use super::*;

    /// Executes an atomic cross-chain arbitrage leg on Solana.
    /// Supports dynamic CPI routing across Raydium AMM V4 and Orca Whirlpools
    /// by consuming accounts passed via `remaining_accounts`.
    pub fn execute_arbitrage(
        ctx: Context<ExecuteArbitrage>,
        amount_in: u64,
        min_amount_out: u64,
        raydium_swap_data: Vec<u8>,
        orca_swap_data: Vec<u8>,
    ) -> Result<()> {
        msg!("Starting Atomic Arbitrage Leg on Solana");
        msg!("Input Amount: {}", amount_in);
        msg!("Minimum Required Output: {}", min_amount_out);

        // 1. Raydium CPI Swap Execution
        // Raydium Swap Base In instruction expects a sequence of accounts.
        // We unpack the first N accounts of remaining_accounts for Raydium.
        let raydium_program_info = &ctx.accounts.raydium_program;
        let mut raydium_accounts = Vec::new();
        
        // Raydium AMM Swap V4 typically takes 18 accounts
        let raydium_acct_count = 18;
        require!(
            ctx.remaining_accounts.len() >= raydium_acct_count,
            ArbError::InvalidRemainingAccounts
        );

        for i in 0..raydium_acct_count {
            let acct = &ctx.remaining_accounts[i];
            raydium_accounts.push(AccountMeta {
                pubkey: acct.key(),
                is_signer: acct.is_signer,
                is_writable: acct.is_writable,
            });
        }

        let raydium_instruction = Instruction {
            program_id: raydium_program_info.key(),
            accounts: raydium_accounts,
            data: raydium_swap_data,
        };

        msg!("Invoking Raydium V4 Swap Base In via CPI...");
        invoke(
            &raydium_instruction,
            &ctx.remaining_accounts[0..raydium_acct_count],
        )?;

        // 2. Orca CLMM Swap Execution
        // Orca Whirlpool Swaps typically expect 9-11 accounts (including tick arrays).
        let orca_program_info = &ctx.accounts.orca_program;
        let orca_start_idx = raydium_acct_count;
        let orca_acct_count = 10;
        
        require!(
            ctx.remaining_accounts.len() >= orca_start_idx + orca_acct_count,
            ArbError::InvalidRemainingAccounts
        );

        let mut orca_accounts = Vec::new();
        for i in 0..orca_acct_count {
            let acct = &ctx.remaining_accounts[orca_start_idx + i];
            orca_accounts.push(AccountMeta {
                pubkey: acct.key(),
                is_signer: acct.is_signer,
                is_writable: acct.is_writable,
            });
        }

        let orca_instruction = Instruction {
            program_id: orca_program_info.key(),
            accounts: orca_accounts,
            data: orca_swap_data,
        };

        msg!("Invoking Orca Whirlpool Swap via CPI...");
        invoke(
            &orca_instruction,
            &ctx.remaining_accounts[orca_start_idx..orca_start_idx + orca_acct_count],
        )?;

        // 3. Verify profitability
        // Reload token account to get updated balances
        ctx.accounts.token_account_out.reload()?;
        let final_balance = ctx.accounts.token_account_out.amount;
        
        require!(
            final_balance >= min_amount_out,
            ArbError::InsufficientProfit
        );

        msg!("Arbitrage executed successfully! Output Balance: {}", final_balance);
        Ok(())
    }

    pub fn receive_wormhole_message(
        _ctx: Context<ReceiveWormholeMessage>,
        payload: Vec<u8>,
    ) -> Result<()> {
        msg!("Received Wormhole Message payload size: {}", payload.len());
        // 1. Verify VAA with Wormhole Core Bridge
        // 2. Decode payload (e.g. cross-chain rebalance signal, atomic state update)
        // 3. Update local state or trigger a Raydium/Orca CPI
        
        msg!("Wormhole payload processed.");
        Ok(())
    }
}

#[derive(Accounts)]
pub struct ReceiveWormholeMessage<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    /// CHECK: Wormhole core bridge program
    pub core_bridge_program: AccountInfo<'info>,
    
    /// CHECK: VAA account provided by the relayer
    pub vaa_account: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct ExecuteArbitrage<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    // Token accounts involved in the arbitrage
    #[account(mut, constraint = token_account_in.owner == owner.key())]
    pub token_account_in: Account<'info, TokenAccount>,
    
    #[account(mut, constraint = token_account_out.owner == owner.key())]
    pub token_account_out: Account<'info, TokenAccount>,

    // Dynamic Swapping Programs
    /// CHECK: Raydium AMM V4 Program
    pub raydium_program: AccountInfo<'info>,

    /// CHECK: Orca Whirlpool Program
    pub orca_program: AccountInfo<'info>,

    // SPL Token Program
    pub token_program: Program<'info, Token>,
}

#[error_code]
pub enum ArbError {
    #[msg("The arbitrage execution did not meet the minimum profit threshold.")]
    InsufficientProfit,
    #[msg("Invalid remaining accounts provided for DEX CPI swaps.")]
    InvalidRemainingAccounts,
}
