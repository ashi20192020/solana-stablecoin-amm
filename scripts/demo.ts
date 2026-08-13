/**
 * End-to-end walkthrough against a local validator with both programs deployed.
 * The Rust/LiteSVM suite remains the authoritative test coverage.
 */
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

// `BN` is only reachable through the CommonJS default export.
import anchor, { AnchorProvider, Program, Wallet, type Idl } from "@anchor-lang/core";
import {
  TOKEN_2022_PROGRAM_ID,
  createInitializeAccount3Instruction,
  getAccount,
  getAccountLenForMint,
  getMint,
} from "@solana/spl-token";
import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
} from "@solana/web3.js";

import type { Amm } from "../target/types/amm.js";
import type { Stablecoin } from "../target/types/stablecoin.js";

const { BN } = anchor;
type BN = anchor.BN;

const RPC_URL = process.env["RPC_URL"] ?? "http://127.0.0.1:8899";
const KEYPAIR_PATH =
  process.env["DEMO_KEYPAIR"] ?? join(homedir(), ".config/solana/id.json");

const DECIMALS = 6;
const UNIT = 10 ** DECIMALS;
const SUPPLY_CAP = new BN(1_000_000_000).muln(UNIT);
const ALLOWANCE = new BN(500_000_000).muln(UNIT);
const FEE_BPS = 30;

const MINT_SEED = Buffer.from("mint");
const CONFIG_SEED = Buffer.from("config");
const MINTER_SEED = Buffer.from("minter");
const POLICY_SEED = Buffer.from("policy");
const POOL_SEED = Buffer.from("pool");
const VAULT_SEED = Buffer.from("vault");
const LP_MINT_SEED = Buffer.from("lp_mint");
const LOCKED_LP_SEED = Buffer.from("locked_lp");

const signatures: Array<[string, string]> = [];

function record(label: string, signature: string): string {
  signatures.push([label, signature]);
  return signature;
}

function loadKeypair(path: string): Keypair {
  const secret = JSON.parse(readFileSync(path, "utf8")) as number[];
  return Keypair.fromSecretKey(Uint8Array.from(secret));
}

function loadIdl<T extends Idl>(name: string): T {
  const url = new URL(`../target/idl/${name}.json`, import.meta.url);
  return JSON.parse(readFileSync(url, "utf8")) as T;
}

function symbolBytes(symbol: string): number[] {
  const bytes = Buffer.alloc(8);
  bytes.write(symbol, "ascii");
  return Array.from(bytes);
}

function expectAddress(label: string, derived: PublicKey, onChain: PublicKey): void {
  if (!derived.equals(onChain)) {
    throw new Error(
      `${label} mismatch: derived ${derived.toBase58()} but the program stored ${onChain.toBase58()}`,
    );
  }
}

class Addresses {
  constructor(
    private readonly stablecoinId: PublicKey,
    private readonly ammId: PublicKey,
  ) {}

  mint(symbol: string): PublicKey {
    return PublicKey.findProgramAddressSync(
      [MINT_SEED, Buffer.from(symbolBytes(symbol))],
      this.stablecoinId,
    )[0];
  }

  config(mint: PublicKey): PublicKey {
    return PublicKey.findProgramAddressSync(
      [CONFIG_SEED, mint.toBuffer()],
      this.stablecoinId,
    )[0];
  }

  minter(config: PublicKey, authority: PublicKey): PublicKey {
    return PublicKey.findProgramAddressSync(
      [MINTER_SEED, config.toBuffer(), authority.toBuffer()],
      this.stablecoinId,
    )[0];
  }

  policy(mint: PublicKey, tokenAccount: PublicKey): PublicKey {
    return PublicKey.findProgramAddressSync(
      [POLICY_SEED, mint.toBuffer(), tokenAccount.toBuffer()],
      this.stablecoinId,
    )[0];
  }

  pool(mintA: PublicKey, mintB: PublicKey): PublicKey {
    return PublicKey.findProgramAddressSync(
      [POOL_SEED, mintA.toBuffer(), mintB.toBuffer()],
      this.ammId,
    )[0];
  }

  vault(pool: PublicKey, mint: PublicKey): PublicKey {
    return PublicKey.findProgramAddressSync(
      [VAULT_SEED, pool.toBuffer(), mint.toBuffer()],
      this.ammId,
    )[0];
  }

  lpMint(pool: PublicKey): PublicKey {
    return PublicKey.findProgramAddressSync(
      [LP_MINT_SEED, pool.toBuffer()],
      this.ammId,
    )[0];
  }

  lockedLp(pool: PublicKey): PublicKey {
    return PublicKey.findProgramAddressSync(
      [LOCKED_LP_SEED, pool.toBuffer()],
      this.ammId,
    )[0];
  }
}

async function createTokenAccount(
  provider: AnchorProvider,
  payer: Keypair,
  mint: PublicKey,
  owner: PublicKey,
): Promise<PublicKey> {
  const mintState = await getMint(
    provider.connection,
    mint,
    undefined,
    TOKEN_2022_PROGRAM_ID,
  );
  // Pausable mints require every account to carry the matching account-side extension.
  const space = getAccountLenForMint(mintState);
  const account = Keypair.generate();
  const transaction = new Transaction().add(
    SystemProgram.createAccount({
      fromPubkey: payer.publicKey,
      newAccountPubkey: account.publicKey,
      space,
      lamports:
        await provider.connection.getMinimumBalanceForRentExemption(space),
      programId: TOKEN_2022_PROGRAM_ID,
    }),
    createInitializeAccount3Instruction(
      account.publicKey,
      mint,
      owner,
      TOKEN_2022_PROGRAM_ID,
    ),
  );
  record(
    `create token account ${account.publicKey.toBase58().slice(0, 8)}`,
    await provider.sendAndConfirm(transaction, [account]),
  );
  return account.publicKey;
}

async function balance(
  provider: AnchorProvider,
  tokenAccount: PublicKey,
): Promise<bigint> {
  const account = await getAccount(
    provider.connection,
    tokenAccount,
    undefined,
    TOKEN_2022_PROGRAM_ID,
  );
  return account.amount;
}

const FEE_DENOMINATOR = 10_000n;
const MINIMUM_LIQUIDITY = 1_000n;
// Callers must bound their own slippage; 99% of the quote leaves room for a
// competing transaction landing first without accepting an arbitrary price.
const SLIPPAGE_BPS = 9_900n;

function bounded(amount: bigint): BN {
  return new BN(((amount * SLIPPAGE_BPS) / FEE_DENOMINATOR).toString());
}

function integerSqrt(value: bigint): bigint {
  if (value < 2n) {
    return value;
  }
  let root = value;
  let next = (root + 1n) / 2n;
  while (next < root) {
    root = next;
    next = (root + value / root) / 2n;
  }
  return root;
}

function quoteInitialLp(amountA: bigint, amountB: bigint): bigint {
  return integerSqrt(amountA * amountB) - MINIMUM_LIQUIDITY;
}

function quoteSwapOut(
  amountIn: bigint,
  reserveIn: bigint,
  reserveOut: bigint,
  feeBps: bigint,
): bigint {
  const inWithFee = amountIn * (FEE_DENOMINATOR - feeBps);
  return (inWithFee * reserveOut) / (reserveIn * FEE_DENOMINATOR + inWithFee);
}

function quoteRemoveOut(
  lpAmount: bigint,
  reserve: bigint,
  lpSupply: bigint,
): bigint {
  return (lpAmount * reserve) / lpSupply;
}

async function reserves(
  program: Program<Amm>,
  pool: PublicKey,
): Promise<{ a: bigint; b: bigint; feeBps: bigint }> {
  const state = await program.account.pool.fetch(pool);
  return {
    a: BigInt(state.reserveA.toString()),
    b: BigInt(state.reserveB.toString()),
    feeBps: BigInt(state.feeBps),
  };
}

function format(amount: bigint | BN): string {
  const value = typeof amount === "bigint" ? amount : BigInt(amount.toString());
  const whole = value / BigInt(UNIT);
  const fraction = (value % BigInt(UNIT)).toString().padStart(DECIMALS, "0");
  return `${whole}.${fraction}`;
}

async function main(): Promise<void> {
  const payer = loadKeypair(KEYPAIR_PATH);
  const connection = new Connection(RPC_URL, "confirmed");
  const provider = new AnchorProvider(connection, new Wallet(payer), {
    commitment: "confirmed",
  });

  const stablecoin = new Program<Stablecoin>(
    loadIdl<Idl>("stablecoin") as unknown as Stablecoin,
    provider,
  );
  const amm = new Program<Amm>(
    loadIdl<Idl>("amm") as unknown as Amm,
    provider,
  );
  const pdas = new Addresses(stablecoin.programId, amm.programId);

  console.log(`rpc            ${RPC_URL}`);
  console.log(`payer          ${payer.publicKey.toBase58()}`);
  console.log(`stablecoin     ${stablecoin.programId.toBase58()}`);
  console.log(`amm            ${amm.programId.toBase58()}`);

  // Two stablecoins, each with the payer as admin, compliance authority, and minter.
  const symbols = ["USDx", "EURx"] as const;
  for (const symbol of symbols) {
    const mint = pdas.mint(symbol);
    const config = pdas.config(mint);
    record(
      `initialize ${symbol}`,
      await stablecoin.methods
        .initializeStablecoin(
          symbolBytes(symbol),
          `Portfolio ${symbol}`,
          `https://example.com/${symbol.toLowerCase()}.json`,
          DECIMALS,
          SUPPLY_CAP,
        )
        .accountsPartial({
          payer: payer.publicKey,
          mint,
          config,
          tokenProgram: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
        })
        .rpc(),
    );
    const state = await stablecoin.account.mintConfig.fetch(config);
    expectAddress(`${symbol} config.mint`, mint, state.mint);

    record(
      `grant minter ${symbol}`,
      await stablecoin.methods
        .grantMinter(ALLOWANCE)
        .accountsPartial({
          admin: payer.publicKey,
          config,
          authority: payer.publicKey,
          minterRole: pdas.minter(config, payer.publicKey),
          systemProgram: SystemProgram.programId,
        })
        .rpc(),
    );
  }

  const usdx = pdas.mint("USDx");
  const eurx = pdas.mint("EURx");
  const [mintA, mintB] =
    Buffer.compare(usdx.toBuffer(), eurx.toBuffer()) < 0
      ? [usdx, eurx]
      : [eurx, usdx];
  const configA = pdas.config(mintA);
  const configB = pdas.config(mintB);

  const userA = await createTokenAccount(provider, payer, mintA, payer.publicKey);
  const userB = await createTokenAccount(provider, payer, mintB, payer.publicKey);

  // Token-2022 defaults every new account to frozen, so the policy must thaw it
  // before any balance can move.
  const allow = async (mint: PublicKey, tokenAccount: PublicKey, label: string) =>
    record(
      `allow ${label}`,
      await stablecoin.methods
        .setWalletPolicy({ allowed: {} })
        .accountsPartial({
          complianceAuthority: payer.publicKey,
          mint,
          config: pdas.config(mint),
          tokenAccount,
          walletPolicy: pdas.policy(mint, tokenAccount),
          tokenProgram: TOKEN_2022_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
        })
        .rpc(),
    );

  await allow(mintA, userA, "user A");
  await allow(mintB, userB, "user B");

  const mintDemo = async (mint: PublicKey, destination: PublicKey, amount: BN) =>
    record(
      `mint ${format(amount)}`,
      await stablecoin.methods
        .mintTo(amount)
        .accountsPartial({
          minter: payer.publicKey,
          mint,
          config: pdas.config(mint),
          minterRole: pdas.minter(pdas.config(mint), payer.publicKey),
          destination,
          walletPolicy: pdas.policy(mint, destination),
          tokenProgram: TOKEN_2022_PROGRAM_ID,
        })
        .rpc(),
    );

  await mintDemo(mintA, userA, new BN(1_000_000).muln(UNIT));
  await mintDemo(mintB, userB, new BN(1_000_000).muln(UNIT));

  const pool = pdas.pool(mintA, mintB);
  const vaultA = pdas.vault(pool, mintA);
  const vaultB = pdas.vault(pool, mintB);
  const lpMint = pdas.lpMint(pool);
  const lockedLp = pdas.lockedLp(pool);

  record(
    "initialize pool",
    await amm.methods
      .initializePool(FEE_BPS)
      .accountsPartial({
        payer: payer.publicKey,
        mintA,
        mintB,
        configA,
        configB,
        pool,
        vaultA,
        vaultB,
        lpMint,
        lockedLp,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .rpc(),
  );

  const poolState = await amm.account.pool.fetch(pool);
  expectAddress("pool.vaultA", vaultA, poolState.vaultA);
  expectAddress("pool.vaultB", vaultB, poolState.vaultB);
  expectAddress("pool.lpMint", lpMint, poolState.lpMint);
  expectAddress("pool.lockedLp", lockedLp, poolState.lockedLp);

  // The vaults are stablecoin accounts too, so they need their own policies.
  await allow(mintA, vaultA, "vault A");
  await allow(mintB, vaultB, "vault B");

  const userLp = await createTokenAccount(provider, payer, lpMint, payer.publicKey);

  const liquidityAccounts = {
    user: payer.publicKey,
    pool,
    mintA,
    mintB,
    configA,
    configB,
    vaultA,
    vaultB,
    lpMint,
    lockedLp,
    userA,
    userB,
    userLp,
    policyA: pdas.policy(mintA, userA),
    policyB: pdas.policy(mintB, userB),
    tokenProgram: TOKEN_2022_PROGRAM_ID,
  };

  // The first deposit is taken exactly as offered, so its minimums are exact.
  const depositA = 200_000n * BigInt(UNIT);
  const depositB = 200_000n * BigInt(UNIT);
  record(
    "add liquidity",
    await amm.methods
      .addLiquidity(
        new BN(depositA.toString()),
        new BN(depositB.toString()),
        new BN(depositA.toString()),
        new BN(depositB.toString()),
        bounded(quoteInitialLp(depositA, depositB)),
      )
      .accountsPartial(liquidityAccounts)
      .rpc(),
  );

  const swapAccounts = {
    user: payer.publicKey,
    pool,
    mintA,
    mintB,
    configA,
    configB,
    vaultA,
    vaultB,
    lpMint,
    userA,
    userB,
    policyA: pdas.policy(mintA, userA),
    policyB: pdas.policy(mintB, userB),
    tokenProgram: TOKEN_2022_PROGRAM_ID,
  };

  const swapAtoB = 10_000n * BigInt(UNIT);
  let live = await reserves(amm, pool);
  record(
    "swap A to B",
    await amm.methods
      .swap(
        { atoB: {} },
        new BN(swapAtoB.toString()),
        bounded(quoteSwapOut(swapAtoB, live.a, live.b, live.feeBps)),
      )
      .accountsPartial(swapAccounts)
      .rpc(),
  );

  const swapBtoA = 5_000n * BigInt(UNIT);
  live = await reserves(amm, pool);
  record(
    "swap B to A",
    await amm.methods
      .swap(
        { btoA: {} },
        new BN(swapBtoA.toString()),
        bounded(quoteSwapOut(swapBtoA, live.b, live.a, live.feeBps)),
      )
      .accountsPartial(swapAccounts)
      .rpc(),
  );

  const burnedLp = (await balance(provider, userLp)) / 2n;
  live = await reserves(amm, pool);
  const lpSupply = (
    await getMint(connection, lpMint, undefined, TOKEN_2022_PROGRAM_ID)
  ).supply;
  record(
    "remove liquidity",
    await amm.methods
      .removeLiquidity(
        new BN(burnedLp.toString()),
        bounded(quoteRemoveOut(burnedLp, live.a, lpSupply)),
        bounded(quoteRemoveOut(burnedLp, live.b, lpSupply)),
      )
      .accountsPartial(liquidityAccounts)
      .rpc(),
  );

  const finalPool = await amm.account.pool.fetch(pool);
  const lpState = await getMint(
    connection,
    lpMint,
    undefined,
    TOKEN_2022_PROGRAM_ID,
  );

  console.log("\nfinal state");
  console.log(`  reserve a      ${format(finalPool.reserveA)}`);
  console.log(`  reserve b      ${format(finalPool.reserveB)}`);
  console.log(`  vault a        ${format(await balance(provider, vaultA))}`);
  console.log(`  vault b        ${format(await balance(provider, vaultB))}`);
  console.log(`  lp supply      ${format(lpState.supply)}`);
  console.log(`  locked lp      ${format(await balance(provider, lockedLp))}`);
  console.log(`  user a         ${format(await balance(provider, userA))}`);
  console.log(`  user b         ${format(await balance(provider, userB))}`);
  console.log(`  user lp        ${format(await balance(provider, userLp))}`);
  console.log(`  fee bps        ${finalPool.feeBps}`);

  console.log("\nsignatures");
  for (const [label, signature] of signatures) {
    console.log(`  ${label.padEnd(30)} ${signature}`);
  }
}

main().catch((error: unknown) => {
  console.error(error);
  process.exitCode = 1;
});
