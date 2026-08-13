use anchor_lang::{InstructionData, ToAccountMetas};
use litesvm::LiteSVM;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

// CARGO_TARGET_TMPDIR is `<target>/tmp`, so the deploy directory is one level up.
const STABLECOIN_SO: &[u8] = include_bytes!(concat!(
    env!("CARGO_TARGET_TMPDIR"),
    "/../deploy/stablecoin.so"
));
const AMM_SO: &[u8] = include_bytes!(concat!(env!("CARGO_TARGET_TMPDIR"), "/../deploy/amm.so"));

fn setup() -> (LiteSVM, Keypair) {
    let mut svm = LiteSVM::new();

    svm.add_program(stablecoin::id(), STABLECOIN_SO)
        .expect("failed to load stablecoin.so; run `anchor build` first");
    svm.add_program(amm::id(), AMM_SO)
        .expect("failed to load amm.so; run `anchor build` first");

    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 1_000_000_000)
        .expect("airdrop to payer failed");

    (svm, payer)
}

fn send(svm: &mut LiteSVM, payer: &Keypair, instruction: Instruction) {
    let message = Message::new_with_blockhash(
        &[instruction],
        Some(&payer.pubkey()),
        &svm.latest_blockhash(),
    );
    let transaction = VersionedTransaction::try_new(VersionedMessage::Legacy(message), &[payer])
        .expect("failed to sign transaction");

    if let Err(failure) = svm.send_transaction(transaction) {
        panic!(
            "health_check failed: {:?}\n{}",
            failure.err,
            failure.meta.logs.join("\n")
        );
    }
}

#[test]
fn stablecoin_health_check_succeeds() {
    let (mut svm, payer) = setup();

    let instruction = Instruction::new_with_bytes(
        stablecoin::id(),
        &stablecoin::instruction::HealthCheck {}.data(),
        stablecoin::accounts::HealthCheck {
            signer: payer.pubkey(),
        }
        .to_account_metas(None),
    );

    send(&mut svm, &payer, instruction);
}

#[test]
fn amm_health_check_succeeds() {
    let (mut svm, payer) = setup();

    let instruction = Instruction::new_with_bytes(
        amm::id(),
        &amm::instruction::HealthCheck {}.data(),
        amm::accounts::HealthCheck {
            signer: payer.pubkey(),
        }
        .to_account_metas(None),
    );

    send(&mut svm, &payer, instruction);
}

#[test]
fn programs_have_distinct_ids() {
    assert_ne!(
        stablecoin::id(),
        amm::id(),
        "stablecoin and amm must not share a program id"
    );
}

#[test]
fn programs_are_executable_after_load() {
    let (svm, _payer) = setup();

    for (name, id) in [("stablecoin", stablecoin::id()), ("amm", amm::id())] {
        let account = svm
            .get_account(&id)
            .unwrap_or_else(|| panic!("{name} program account missing after load"));
        assert!(
            account.executable,
            "{name} program account is not executable"
        );
    }
}
