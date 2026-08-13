use anchor_lang::prelude::*;
use anchor_spl::{
    token_2022::{
        freeze_account,
        spl_token_2022::{
            extension::StateWithExtensions,
            state::{Account as TokenAccountState, AccountState},
        },
        thaw_account, FreezeAccount, ThawAccount, Token2022,
    },
    token_interface::{Mint, TokenAccount},
};

use crate::{
    constants::{CONFIG_SEED, POLICY_SEED},
    errors::StablecoinError,
    state::{MintConfig, PolicyStatus, WalletPolicy},
};

#[derive(Accounts)]
pub struct SetWalletPolicy<'info> {
    #[account(mut)]
    pub compliance_authority: Signer<'info>,

    #[account(owner = anchor_spl::token_2022::ID)]
    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        seeds = [CONFIG_SEED, mint.key().as_ref()],
        bump = config.bump,
        has_one = mint,
        has_one = compliance_authority @ StablecoinError::Unauthorized,
    )]
    pub config: Account<'info, MintConfig>,

    #[account(
        mut,
        owner = anchor_spl::token_2022::ID,
        constraint = token_account.mint == mint.key() @ StablecoinError::TokenAccountMintMismatch,
    )]
    pub token_account: InterfaceAccount<'info, TokenAccount>,

    #[account(
        init_if_needed,
        payer = compliance_authority,
        space = WalletPolicy::DISCRIMINATOR.len() + WalletPolicy::INIT_SPACE,
        seeds = [POLICY_SEED, mint.key().as_ref(), token_account.key().as_ref()],
        bump,
    )]
    pub wallet_policy: Account<'info, WalletPolicy>,

    pub token_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

pub(crate) fn handler(ctx: Context<SetWalletPolicy>, status: PolicyStatus) -> Result<()> {
    let config = &ctx.accounts.config;
    let config_seeds: &[&[u8]] = &[CONFIG_SEED, config.mint.as_ref(), &[config.bump]];
    let token_account = ctx.accounts.token_account.to_account_info();

    // Token-2022 rejects freezing an already frozen account and thawing a thawed one,
    // so the CPI runs only when the current state differs from the requested policy.
    if status.is_blocked() != is_frozen(&token_account)? {
        let account = token_account.clone();
        let mint = ctx.accounts.mint.to_account_info();
        let authority = config.to_account_info();
        let program_id = ctx.accounts.token_program.key();

        if status.is_blocked() {
            freeze_account(CpiContext::new_with_signer(
                program_id,
                FreezeAccount {
                    account,
                    mint,
                    authority,
                },
                &[config_seeds],
            ))?;
        } else {
            thaw_account(CpiContext::new_with_signer(
                program_id,
                ThawAccount {
                    account,
                    mint,
                    authority,
                },
                &[config_seeds],
            ))?;
        }
    }

    require!(
        is_frozen(&token_account)? == status.is_blocked(),
        StablecoinError::PolicyStateMismatch
    );

    let bump = ctx.bumps.wallet_policy;
    ctx.accounts.wallet_policy.set_inner(WalletPolicy {
        mint: ctx.accounts.mint.key(),
        token_account: ctx.accounts.token_account.key(),
        owner: ctx.accounts.token_account.owner,
        status,
        updated_at: Clock::get()?.unix_timestamp,
        updated_by: ctx.accounts.compliance_authority.key(),
        bump,
        _reserved: [0; 32],
    });

    Ok(())
}

fn is_frozen(token_account: &AccountInfo<'_>) -> Result<bool> {
    let data = token_account.try_borrow_data()?;
    let state = StateWithExtensions::<TokenAccountState>::unpack(&data)?;
    Ok(state.base.state == AccountState::Frozen)
}
