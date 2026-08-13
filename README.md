# solana-stablecoin-amm

A permissioned stablecoin issuer and a constant-product AMM for trading the issued
stablecoins against each other, written in Rust with Anchor and Token-2022.

The design question it explores is the one institutional digital-asset issuers actually
face: a regulated stablecoin needs allowlists, freezes, a global pause, and supply caps,
but a pool needs to hold and move balances permissionlessly. The two programs here draw
that boundary explicitly — compliance state lives with the issuer, and the AMM reads it
as a precondition on every value-moving instruction rather than re-implementing it.

## Architecture

Two on-chain programs, deployed separately:

- **`stablecoin`** — issues Token-2022 mints, tracks per-wallet policy, minter allowances,
  supply counters, pause state, and a two-step admin rotation.
- **`amm`** — one pool per ordered mint pair, holding both vaults, the LP mint, and a
  permanently locked LP balance. It has no administrator and no upgradable parameters; the
  fee is fixed at pool creation.

The AMM depends on the stablecoin crate only for its account layouts and seed constants,
so it can re-derive and validate `MintConfig` and `WalletPolicy` PDAs itself. It never
calls the stablecoin program.

## Instructions

Thirteen instructions in total.

| `stablecoin` | Purpose |
| --- | --- |
| `initialize_stablecoin` | Create the mint PDA, its extensions, and `MintConfig` |
| `grant_minter` | Open a `MinterRole` with a fixed allowance |
| `revoke_minter` | Close the role and refund rent |
| `set_wallet_policy` | Allow or block a token account, thawing or freezing it |
| `mint_to` | Mint within allowance, supply cap, and policy |
| `burn` | Burn from an account the signer owns |
| `set_pause` | Pause or resume the mint through the Pausable extension |
| `update_config` | Change supply cap, compliance authority, or nominate an admin |
| `accept_admin` | Complete an admin rotation |

| `amm` | Purpose |
| --- | --- |
| `initialize_pool` | Create the pool, vaults, LP mint, and locked LP account |
| `add_liquidity` | Deposit both sides and receive LP tokens |
| `remove_liquidity` | Burn LP tokens and withdraw both sides |
| `swap` | Trade one stablecoin for the other |

## Token-2022 and the authority model

Each stablecoin mint is a PDA at `["mint", symbol]` carrying four extensions:

- **`DefaultAccountState = Frozen`** — every new token account is frozen on creation, so a
  wallet cannot hold or move the stablecoin until compliance thaws it. This is what makes
  the allowlist a default-deny rather than a default-allow.
- **`Pausable`** — a global halt that Token-2022 itself enforces on transfers, mints, and
  burns, so the pause holds even for callers that never touch this program.
- **`MetadataPointer`** and **`TokenMetadata`** — metadata stored in the mint itself.

Every mint-level authority — mint, freeze, pause, and metadata update — is the `MintConfig`
PDA at `["config", mint]`. No human key ever holds them; all privileged actions go through
the program, where they are checked against `admin` or `compliance_authority`. Admin
rotation is two-step (`update_config` nominates, `accept_admin` completes), so a typo
cannot strand the mint.

Pool child accounts are PDAs of the pool, and the pool PDA is the authority for both
vaults, the LP mint, and the locked LP account. The LP mint has no freeze authority.

## AMM math

Constant product, `x · y = k`, with a fee taken from the input:

```
amount_out = (amount_in · (10000 − fee_bps) · reserve_out)
             ÷ (reserve_in · 10000 + amount_in · (10000 − fee_bps))
```

`fee_bps` is fixed at pool creation and capped at 100 (1%). There is no protocol fee; the
entire fee accrues to liquidity providers as growth in `k`.

The first deposit mints `sqrt(a · b)` LP tokens and permanently locks `MINIMUM_LIQUIDITY`
(1,000) of them in a pool-owned account, so the LP supply can never return to zero and the
share price cannot be manipulated by draining the pool to dust. Later deposits mint
`min(a · L / reserve_a, b · L / reserve_b)` and pull the matching amount of the other side.

All intermediates are `u128` with checked arithmetic, narrowed back to `u64` through a
fallible conversion. There are no floats and no `as` casts. Every rounding decision favours
the pool: deposits round the required amount up, withdrawals and swap outputs round down.
Callers protect themselves with explicit minimums (`amount_a_min`, `min_lp_out`,
`min_amount_out`) on every value-moving instruction.

## Security invariants

Enforced on-chain, and re-asserted after every operation by the deterministic sequence test:

- The live Token-2022 mint supply is authoritative, and `mint.supply ≤ supply_cap ≤
  MAX_SUPPLY_CAP` is enforced against it. `total_minted` covers all issuance, because only the
  config PDA holds mint authority, while `total_burned` only tracks burns routed through this
  program. An owner may burn a thawed balance through Token-2022 directly, so tracked
  outstanding supply is an upper bound: `total_minted − total_burned ≥ mint.supply`.
- Stored pause state always equals the live `PausableConfig`; drift is rejected rather than
  silently corrected.
- Each wallet policy agrees with the token account's actual frozen state.
- Every minter's cumulative `minted` stays within its allowance.
- Each vault balance is at least the reserve the pool has recorded, so donated surplus can
  never be counted as liquidity.
- The live LP mint supply is the single source of truth; the pool stores no LP supply of its
  own, so an LP holder burning tokens outside the program cannot desynchronise it.
- The locked LP balance stays between `MINIMUM_LIQUIDITY` and the live supply, and no
  withdrawal may push the supply below the balance actually locked.
- Swaps never decrease `k`, and across successful operations normalised pool value never
  falls: `k_new · L_old² ≥ k_old · L_new²`.

### Threat-model decisions

- **Pre-funded PDAs.** Every canonical address is public, so anyone can send lamports to one
  before it is created. Both programs top up, allocate, and assign such an address instead of
  failing, and reject it only when it is genuinely occupied. Otherwise a few lamports would
  permanently block a mint or a pool.
- **Donations.** Stablecoins sent directly to a vault are ignored, and LP tokens sent to the
  locked account are accepted. Neither can brick the pool or be withdrawn.
- **Live child accounts.** The AMM validates canonical addresses *and* the accounts' live
  contents — vault mints and authorities, LP mint decimals, authority, and absence of a
  freeze authority — so a corrupted child cannot be used to move value.
- **Error codes are append-only.** New variants go at the end of their enum so existing
  numbers never shift.

## Accepted limitations (v1)

These are deliberate, not oversights:

- **LP tokens are freely transferable.** The LP mint carries no policy or freeze authority,
  so a blocked wallet can still hold LP tokens. Only the underlying stablecoin legs are
  compliance-gated.
- **Sandwich and MEV exposure.** There is no oracle, TWAP, or private ordering. Slippage
  bounds are the only protection, and callers must set them.
- **Donated vault surplus is stranded.** Tokens transferred straight into a vault are never
  credited to reserves and cannot be recovered.
- **Pause blocks withdrawals.** A paused stablecoin halts `remove_liquidity` as well as
  swaps. Token-2022 would block the transfer regardless, so the program fails early rather
  than pretending otherwise.
- **Mints are seeded by symbol globally.** `["mint", symbol]` means one `USDx` per program
  deployment, first-come-first-served.
- **The upgrade authority is a trust root.** Both programs are upgradeable, so whoever holds
  that key can replace all of the above.

## Toolchain

| Tool | Version |
| --- | --- |
| Rust | 1.89.0 |
| Anchor | 1.1.2 |
| Solana / Agave | 3.1.10 |
| LiteSVM | 0.12.0 |
| Node | ≥ 20.11.0 |

Dependencies are exact-pinned (`=x.y.z`) in every manifest.

## Build and test

```sh
cargo fmt --all --check
cargo clippy --workspace --exclude integration-tests --all-targets --all-features -- -D warnings
anchor build            # must precede the integration tests; they include_bytes! the .so files
cargo clippy -p integration-tests --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Focused coverage for the pure math module:

```sh
cargo llvm-cov --package amm --lib --all-features --json --output-path math-coverage.json
```

## Local TypeScript demo

A single script that walks the whole protocol against a local validator. The Rust suite is
the authoritative coverage; this exists to show the client-side shape of the programs.

Build first, then start a validator with both programs loaded at their declared addresses:

```sh
anchor build

solana-test-validator --reset \
  --bpf-program 6GuaNWi16p2a1T6jChe2d2cC2SjKDiCoiHN14tciMzE1 target/deploy/stablecoin.so \
  --bpf-program F8JkSibw1r9bfqj1XGaomozVbTq5YPg7L7zqJWKv7evU target/deploy/amm.so
```

In another shell:

```sh
solana airdrop 100 --url http://127.0.0.1:8899
npm --prefix scripts ci
npm --prefix scripts run typecheck
npm --prefix scripts run demo
```

The demo derives every PDA locally, verifies them against what the programs stored,
initializes USDx and EURx, allowlists accounts, mints balances, creates the pool, adds
liquidity, swaps both ways, removes liquidity, and prints the resulting balances, reserves,
LP supply, and transaction signatures. It reads the payer from
`~/.config/solana/id.json`; override with `DEMO_KEYPAIR` and `RPC_URL`.

## Testing strategy

- **Property tests** (`proptest`) over the pure math module cover the full `u128` domain,
  the `k` invariant, rounding direction, and overflow safety.
- **LiteSVM integration tests** run the real compiled `.so` files, covering every
  instruction's success and failure paths, cross-program pause and policy interaction,
  account substitution and corruption, pre-funded PDAs, and emitted events.
- **A deterministic invariant sequence** runs 180+ operations across several actors —
  deposits, withdrawals, swaps in both directions, pause and policy transitions, and
  operations that must be rejected — asserting every protocol invariant after each step,
  including that rejected operations leave state untouched.

CI runs formatting, both clippy passes, the Anchor build, the full test suite, the focused
math coverage gate, and the demo client's typecheck. GitHub Actions are SHA-pinned,
permissions are read-only, and in-progress runs are cancelled on new pushes.

## Disclaimer

This is an educational portfolio project. It has not been audited and is not production
ready. Do not deploy it with real value.

Public AMM and stablecoin implementations informed the investigation behind this design,
but the implementation here is original.
