use anchor_lang::prelude::*;

use crate::math::MathError;

#[error_code]
pub enum AmmError {
    #[msg("both pool mints must be distinct")]
    IdenticalMints,
    #[msg("pool mints must be supplied in ascending bytewise order")]
    InvalidMintOrder,
    #[msg("fee exceeds the protocol maximum")]
    FeeTooHigh,
    #[msg("pool mints must share the protocol stablecoin decimals")]
    DecimalsMismatch,
    #[msg("mint carries an extension this pool does not support")]
    UnsupportedMintExtension,
    #[msg("stablecoin config does not describe the supplied mint")]
    InvalidStablecoinConfig,
    #[msg("stablecoin protocol is paused")]
    ProtocolPaused,
    #[msg("wallet policy does not allow this token account")]
    WalletNotAllowed,
    #[msg("quoted amount violates the supplied slippage bound")]
    SlippageExceeded,
    #[msg("vault balance is below the stored reserve")]
    VaultBalanceInvariant,
    #[msg("lp mint supply does not match the expected result of this operation")]
    LpSupplyInvariant,
    #[msg("locked liquidity is outside the permanently locked bounds")]
    LockedLiquidityInvariant,
    #[msg("canonical child account is already in use")]
    ChildAccountInUse,
    #[msg("the same token account was supplied twice")]
    DuplicateAccount,
    #[msg("token account does not belong to the expected mint")]
    TokenAccountMintMismatch,
    #[msg("token account is not owned by the signer")]
    InvalidTokenOwner,
    #[msg("swap would decrease the constant product")]
    ConstantProductViolation,
    #[msg("amount must be greater than zero")]
    ZeroAmount,
    #[msg("fee is outside the supported range")]
    InvalidFee,
    #[msg("pool does not hold enough liquidity")]
    InsufficientLiquidity,
    #[msg("initial deposit does not exceed the permanently locked minimum")]
    InsufficientInitialLiquidity,
    #[msg("quoted output is zero")]
    ZeroOutput,
    #[msg("pool liquidity state is invalid for this operation")]
    InvalidLiquidityState,
    #[msg("value is outside the supported protocol range")]
    ValueOutOfRange,
    #[msg("arithmetic overflow")]
    MathOverflow,
}

impl From<MathError> for AmmError {
    fn from(error: MathError) -> Self {
        match error {
            MathError::ZeroAmount => Self::ZeroAmount,
            MathError::InvalidFee => Self::InvalidFee,
            MathError::InsufficientLiquidity => Self::InsufficientLiquidity,
            MathError::InsufficientInitialLiquidity => Self::InsufficientInitialLiquidity,
            MathError::ZeroOutput => Self::ZeroOutput,
            MathError::InvalidLiquidityState => Self::InvalidLiquidityState,
            MathError::ValueOutOfRange => Self::ValueOutOfRange,
            MathError::MathOverflow => Self::MathOverflow,
        }
    }
}
