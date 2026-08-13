use anchor_lang::prelude::*;

#[error_code]
pub enum StablecoinError {
    #[msg("decimals must equal the protocol stablecoin decimals")]
    UnsupportedDecimals,
    #[msg("supply cap must be greater than zero")]
    ZeroSupplyCap,
    #[msg("supply cap exceeds the protocol maximum")]
    SupplyCapTooLarge,
    #[msg("symbol must be non-empty ASCII alphanumeric right-padded with zero bytes")]
    InvalidSymbol,
    #[msg("name must be non-empty and within the maximum length")]
    InvalidName,
    #[msg("uri must be non-empty and within the maximum length")]
    InvalidUri,
    #[msg("arithmetic overflow")]
    MathOverflow,
    #[msg("mint account is not rent exempt")]
    MintNotRentExempt,
    #[msg("signer is not authorized for this action")]
    Unauthorized,
    #[msg("amount must be greater than zero")]
    ZeroAmount,
    #[msg("minter authority must not be the default pubkey")]
    InvalidAuthority,
    #[msg("allowance must be greater than zero")]
    ZeroAllowance,
    #[msg("mint would exceed the minter allowance")]
    AllowanceExceeded,
    #[msg("mint would exceed the configured supply cap")]
    SupplyCapExceeded,
    #[msg("wallet policy does not allow this token account")]
    WalletNotAllowed,
    #[msg("protocol is paused")]
    ProtocolPaused,
    #[msg("tracked outstanding supply is below the live mint supply")]
    CounterInvariantViolation,
    #[msg("token account does not belong to this mint")]
    TokenAccountMintMismatch,
    #[msg("token account frozen state does not match the stored wallet policy")]
    PolicyStateMismatch,
    #[msg("mint pause extension does not match the mirrored config pause flag")]
    PauseStateDrift,
    #[msg("mint is already in the requested pause state")]
    PauseStateUnchanged,
    #[msg("supply cap must be at least the current mint supply")]
    SupplyCapBelowSupply,
    #[msg("no configuration change was requested")]
    NoConfigChange,
    #[msg("pending admin must not be the default pubkey or the current admin")]
    InvalidPendingAdmin,
    #[msg("signer is not the pending admin")]
    NoPendingAdmin,
    #[msg("canonical mint account is already in use")]
    MintAccountInUse,
}
