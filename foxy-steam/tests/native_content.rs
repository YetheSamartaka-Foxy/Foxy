use std::time::Duration;

use foxy_steam::auth::{self, AuthPhase, LoginInput};
use foxy_steam::content::{self, ContentError, Platform};
use tokio::sync::{mpsc, watch};

const REDIST_APP: u32 = 1007;
const ARMA3_SERVER_APP: u32 = 233_780;

fn platform() -> Platform {
    if cfg!(target_os = "linux") {
        Platform::LinuxX64
    } else {
        Platform::WindowsX64
    }
}

async fn anonymous() -> auth::AuthenticatedSession {
    let (phase_tx, _phase_rx) = watch::channel(AuthPhase::Connecting);
    let (_code_tx, code_rx) = mpsc::channel(1);
    let (_cancel_tx, cancel_rx) = watch::channel(false);
    auth::authenticate(LoginInput::Anonymous, phase_tx, code_rx, cancel_rx)
        .await
        .expect("anonymous native Steam logon")
}

#[tokio::test]
#[ignore = "contacts Steam and downloads a small anonymous redistributable"]
async fn native_anonymous_review_install_verify_and_cancel() {
    let session = anonymous().await;

    let arma = content::review(
        &session.client,
        ARMA3_SERVER_APP,
        "public",
        platform(),
        true,
    )
    .await
    .expect("anonymous app info review");
    assert!(
        !arma.selection.depots.is_empty(),
        "no Arma server depots selected"
    );
    assert!(
        !arma.entitled,
        "anonymous session unexpectedly entitled to the Arma server"
    );
    println!(
        "arma3 server public build={:?} depots={:?} skipped={}",
        arma.selection.build_id,
        arma.selection
            .depots
            .iter()
            .map(|depot| (depot.depot_id, depot.size))
            .collect::<Vec<_>>(),
        arma.selection.skipped.len()
    );

    let review = content::review(&session.client, REDIST_APP, "public", platform(), true)
        .await
        .expect("redistributable review");
    assert!(
        review.entitled,
        "anonymous session cannot fetch redistributable keys"
    );

    let root = tempfile::tempdir().unwrap();
    let staging = root.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let report = content::install(&session.client, &review, &staging, events_tx, 8)
        .await
        .expect("redistributable install");
    let mut events = 0usize;
    while events_rx.try_recv().is_ok() {
        events += 1;
    }
    assert!(events > 0, "no download events were emitted");
    let verified = match content::verify_tree(&staging, &report.entries, &|| false) {
        Ok(verified) => verified,
        Err(error) => {
            let mut stack = vec![staging.clone()];
            while let Some(directory) = stack.pop() {
                for entry in std::fs::read_dir(&directory).unwrap() {
                    let entry = entry.unwrap();
                    let relative = entry
                        .path()
                        .strip_prefix(&staging)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    let kind = entry.file_type().unwrap();
                    let known = report.entries.get(&relative);
                    let size = entry.metadata().unwrap().len();
                    if known.is_none()
                        || known.is_some_and(|known| !known.directory && known.size != size)
                    {
                        println!(
                            "disk {relative} dir={} size={size} expected={known:?}",
                            kind.is_dir()
                        );
                    }
                    if kind.is_dir() {
                        stack.push(entry.path());
                    }
                }
            }
            for (path, entry) in &report.entries {
                if !staging.join(path).exists() {
                    println!("missing {path} {entry:?}");
                }
            }
            panic!("staged tree verifies against manifests: {error:?}");
        }
    };
    let bytes: u64 = verified.iter().map(|file| file.bytes).sum();
    assert!(!verified.is_empty());
    println!(
        "redist depots={} files={} bytes={bytes} events={events}",
        report.depots.len(),
        verified.len()
    );

    std::fs::write(staging.join("unexpected.txt"), b"x").unwrap();
    assert_eq!(
        content::verify_tree(&staging, &report.entries, &|| false),
        Err(ContentError::VerificationFailed)
    );

    let cancelled_staging = root.path().join("cancelled");
    std::fs::create_dir(&cancelled_staging).unwrap();
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let install = content::cancellable(
        cancel_rx,
        Duration::from_secs(300),
        content::install(&session.client, &review, &cancelled_staging, events_tx, 1),
    );
    let cancel = async {
        events_rx.recv().await;
        cancel_tx.send_replace(true);
    };
    let (result, ()) = tokio::join!(install, cancel);
    assert_eq!(result, Err(ContentError::Cancelled));
    drop(session);
}

#[tokio::test]
#[ignore = "contacts Steam; set FOXY_NATIVE_WORKSHOP_ITEMS to public Arma 3 Workshop item ids"]
async fn native_anonymous_workshop_details_and_refused_download() {
    use foxy_steam::workshop;

    const ARMA3_CLIENT_APP: u32 = 107_410;
    let ids: Vec<u64> = std::env::var("FOXY_NATIVE_WORKSHOP_ITEMS")
        .expect("FOXY_NATIVE_WORKSHOP_ITEMS is required")
        .split(',')
        .map(|id| id.trim().parse().expect("numeric Workshop id"))
        .collect();
    let missing = u64::MAX - 1;
    let mut requested = ids.clone();
    requested.push(missing);
    let session = anonymous().await;
    let items = workshop::item_details(&session.client, &requested)
        .await
        .expect("published file details");
    assert_eq!(items.len(), requested.len());
    for item in &items[..ids.len()] {
        let manifest = workshop::eligible(item, ARMA3_CLIENT_APP).expect("eligible public item");
        println!(
            "item consumer_app={} manifest_present={} size={} updated={}",
            item.consumer_app_id,
            manifest != 0,
            item.file_size,
            item.time_updated
        );
    }
    assert_eq!(
        workshop::eligible(items.last().unwrap(), ARMA3_CLIENT_APP),
        Err(workshop::ItemIssue::NotFound)
    );
    assert!(
        !workshop::ugc_entitled(&session.client, ARMA3_CLIENT_APP)
            .await
            .unwrap()
    );

    let servers = content::cdn_servers(&session.client).await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let (events_tx, _events_rx) = mpsc::unbounded_channel();
    let refused = workshop::download_item(
        &session.client,
        &servers,
        ARMA3_CLIENT_APP,
        workshop::eligible(&items[0], ARMA3_CLIENT_APP).unwrap(),
        root.path(),
        events_tx,
        4,
    )
    .await;
    assert_eq!(refused.err(), Some(ContentError::EntitlementRequired));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
