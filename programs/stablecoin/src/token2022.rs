use anchor_lang::{prelude::*, solana_program::program::invoke};
use anchor_spl::token_2022::spl_token_2022::extension::pausable;

/// anchor-spl ships no Pausable wrapper, so the raw Token-2022 instruction is built here.
pub fn pausable_initialize<'info>(
    token_program: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    authority: &Pubkey,
) -> Result<()> {
    let ix = pausable::instruction::initialize(token_program.key, mint.key, authority)?;
    invoke(&ix, &[token_program.clone(), mint.clone()]).map_err(Into::into)
}
