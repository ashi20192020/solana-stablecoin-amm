use anchor_lang::{
    prelude::*,
    system_program::{create_account, CreateAccount},
};
use anchor_spl::{
    token_2022::{
        initialize_mint2,
        spl_token_2022::{
            extension::ExtensionType,
            state::{AccountState, Mint as MintState},
        },
        InitializeMint2, Token2022,
    },
    token_2022_extensions::{
        default_account_state_initialize, metadata_pointer_initialize,
        spl_pod::optional_keys::OptionalNonZeroPubkey,
        spl_token_metadata_interface::state::TokenMetadata, token_metadata_initialize,
        DefaultAccountStateInitialize, MetadataPointerInitialize, TokenMetadataInitialize,
    },
};

use crate::{
    constants::{
        CONFIG_SEED, MAX_NAME_LEN, MAX_SUPPLY_CAP, MAX_URI_LEN, MINT_SEED, STABLECOIN_DECIMALS,
    },
    errors::StablecoinError,
    state::MintConfig,
    token2022::pausable_initialize,
};

const MINT_EXTENSIONS: [ExtensionType; 3] = [
    ExtensionType::DefaultAccountState,
    ExtensionType::Pausable,
    ExtensionType::MetadataPointer,
];

#[derive(Accounts)]
#[instruction(symbol: [u8; 8])]
pub struct InitializeStablecoin<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: allocated and initialized as a Token-2022 mint below; the address is
    /// constrained to the canonical PDA and the account must be uninitialized.
    #[account(mut, seeds = [MINT_SEED, symbol.as_ref()], bump)]
    pub mint: UncheckedAccount<'info>,

    #[account(
        init,
        payer = payer,
        space = MintConfig::DISCRIMINATOR.len() + MintConfig::INIT_SPACE,
        seeds = [CONFIG_SEED, mint.key().as_ref()],
        bump,
    )]
    pub config: Account<'info, MintConfig>,

    pub token_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

pub fn handler(
    ctx: Context<InitializeStablecoin>,
    symbol: [u8; 8],
    name: String,
    uri: String,
    decimals: u8,
    supply_cap: u64,
) -> Result<()> {
    require!(
        decimals == STABLECOIN_DECIMALS,
        StablecoinError::UnsupportedDecimals
    );
    require!(supply_cap > 0, StablecoinError::ZeroSupplyCap);
    require!(
        supply_cap <= MAX_SUPPLY_CAP,
        StablecoinError::SupplyCapTooLarge
    );
    require!(
        !name.is_empty() && name.len() <= MAX_NAME_LEN,
        StablecoinError::InvalidName
    );
    require!(
        !uri.is_empty() && uri.len() <= MAX_URI_LEN,
        StablecoinError::InvalidUri
    );
    let symbol_text = normalize_symbol(&symbol)?;

    let mint_key = ctx.accounts.mint.key();
    let config_key = ctx.accounts.config.key();
    let token_program_id = ctx.accounts.token_program.key();

    let base_len = ExtensionType::try_calculate_account_len::<MintState>(&MINT_EXTENSIONS)?;
    let metadata = TokenMetadata {
        update_authority: OptionalNonZeroPubkey::try_from(Some(config_key))?,
        mint: mint_key,
        name: name.clone(),
        symbol: symbol_text.clone(),
        uri: uri.clone(),
        additional_metadata: Vec::new(),
    };
    // Token metadata is variable length and appended by a later CPI that grows the mint
    // without funding it, so the account is created at `base_len` but funded for `final_len`.
    let final_len = base_len
        .checked_add(metadata.tlv_size_of()?)
        .ok_or(StablecoinError::MathOverflow)?;
    let rent = Rent::get()?;

    let mint_bump = ctx.bumps.mint;
    let mint_seeds: &[&[u8]] = &[MINT_SEED, symbol.as_ref(), &[mint_bump]];
    create_account(
        CpiContext::new(
            ctx.accounts.system_program.key(),
            CreateAccount {
                from: ctx.accounts.payer.to_account_info(),
                to: ctx.accounts.mint.to_account_info(),
            },
        )
        .with_signer(&[mint_seeds]),
        rent.minimum_balance(final_len),
        u64::try_from(base_len).map_err(|_| StablecoinError::MathOverflow)?,
        &token_program_id,
    )?;

    default_account_state_initialize(
        CpiContext::new(
            token_program_id,
            DefaultAccountStateInitialize {
                token_program_id: ctx.accounts.token_program.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
            },
        ),
        &AccountState::Frozen,
    )?;
    pausable_initialize(
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.mint.to_account_info(),
        &config_key,
    )?;
    metadata_pointer_initialize(
        CpiContext::new(
            token_program_id,
            MetadataPointerInitialize {
                token_program_id: ctx.accounts.token_program.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
            },
        ),
        Some(config_key),
        Some(mint_key),
    )?;

    initialize_mint2(
        CpiContext::new(
            token_program_id,
            InitializeMint2 {
                mint: ctx.accounts.mint.to_account_info(),
            },
        ),
        decimals,
        &config_key,
        Some(&config_key),
    )?;

    let config_bump = ctx.bumps.config;
    let config_seeds: &[&[u8]] = &[CONFIG_SEED, mint_key.as_ref(), &[config_bump]];
    token_metadata_initialize(
        CpiContext::new(
            token_program_id,
            TokenMetadataInitialize {
                program_id: ctx.accounts.token_program.to_account_info(),
                metadata: ctx.accounts.mint.to_account_info(),
                update_authority: ctx.accounts.config.to_account_info(),
                mint_authority: ctx.accounts.config.to_account_info(),
                mint: ctx.accounts.mint.to_account_info(),
            },
        )
        .with_signer(&[config_seeds]),
        name,
        symbol_text,
        uri,
    )?;

    let mint_info = ctx.accounts.mint.to_account_info();
    require!(
        rent.is_exempt(mint_info.lamports(), mint_info.data_len()),
        StablecoinError::MintNotRentExempt
    );

    let payer_key = ctx.accounts.payer.key();
    ctx.accounts.config.set_inner(MintConfig {
        admin: payer_key,
        pending_admin: None,
        compliance_authority: payer_key,
        mint: mint_key,
        symbol,
        decimals,
        supply_cap,
        total_minted: 0,
        total_burned: 0,
        paused: false,
        bump: config_bump,
        mint_bump,
        _reserved: [0; 64],
    });

    Ok(())
}

fn normalize_symbol(symbol: &[u8; 8]) -> Result<String> {
    let end = symbol
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(symbol.len());
    let (text, padding) = symbol.split_at(end);
    require!(
        !text.is_empty()
            && text.iter().all(u8::is_ascii_alphanumeric)
            && padding.iter().all(|byte| *byte == 0),
        StablecoinError::InvalidSymbol
    );
    String::from_utf8(text.to_vec()).map_err(|_| StablecoinError::InvalidSymbol.into())
}
