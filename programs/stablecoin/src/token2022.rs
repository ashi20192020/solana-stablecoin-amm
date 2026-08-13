use anchor_lang::{
    prelude::*,
    solana_program::program::{invoke, invoke_signed},
};
use anchor_spl::token_2022::spl_token_2022::{
    extension::{
        pausable::{self, PausableConfig},
        BaseStateWithExtensions, StateWithExtensions,
    },
    state::Mint as MintState,
};

/// anchor-spl ships no Pausable wrappers, so the raw Token-2022 instructions are built here.
pub fn pausable_initialize<'info>(
    token_program: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    authority: &Pubkey,
) -> Result<()> {
    let ix = pausable::instruction::initialize(token_program.key, mint.key, authority)?;
    invoke(&ix, &[token_program.clone(), mint.clone()]).map_err(Into::into)
}

pub fn pausable_set<'info>(
    token_program: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    paused: bool,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    let build = if paused {
        pausable::instruction::pause
    } else {
        pausable::instruction::resume
    };
    let ix = build(token_program.key, mint.key, authority.key, &[])?;
    invoke_signed(
        &ix,
        &[token_program.clone(), mint.clone(), authority.clone()],
        signer_seeds,
    )
    .map_err(Into::into)
}

pub fn pausable_is_paused(mint: &AccountInfo<'_>) -> Result<bool> {
    let data = mint.try_borrow_data()?;
    let state = StateWithExtensions::<MintState>::unpack(&data)?;
    Ok(bool::from(state.get_extension::<PausableConfig>()?.paused))
}
