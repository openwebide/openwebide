use super::*;
use crate::folder_coordination::Folder;

#[tokio::test]
async fn companion_proof_creation_waits_for_an_accepted_native_folder_operation() {
    let case = Case::new();
    let lease = case
        .host
        .coordinator
        .acquire(Folder::capture(&case.root()).unwrap())
        .await
        .unwrap();
    let host = case.host.clone();
    let root = case.root();
    let request = tokio::spawn(async move {
        host.request(
            root,
            "paired/user:1".into(),
            FolderRequest {
                user: 1,
                project: 1,
                action: FolderAction::Open {
                    path: "".into(),
                    expected: None,
                },
            },
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while case.host.coordinator.waiting() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read_dir(case.root()).unwrap().count(), 0);
    drop(lease);
    let FolderReply::Opened { session, .. } = request.await.unwrap().unwrap() else {
        panic!()
    };
    case.request(FolderAction::Close { session }).await.unwrap();
    assert_eq!(std::fs::read_dir(case.root()).unwrap().count(), 0);
}

#[tokio::test]
async fn a_new_nested_proof_fences_an_older_parent_even_across_accounts_and_projects() {
    let case = Case::new();
    std::fs::create_dir(case.root().join("child")).unwrap();
    std::fs::write(case.root().join("child/source"), "unchanged").unwrap();
    let (parent, challenge, _) = case.open().await;
    case.prove(&parent, &challenge).await;
    let request = |action| FolderRequest {
        user: 2,
        project: 2,
        action,
    };
    let FolderReply::Opened {
        session, challenge, ..
    } = case
        .host
        .request(
            case.root(),
            "paired/user:2".into(),
            request(FolderAction::Open {
                path: "child".into(),
                expected: None,
            }),
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    let proof = std::fs::read_to_string(case.root().join("child").join(challenge)).unwrap();
    case.host
        .request(
            case.root(),
            "paired/user:2".into(),
            request(FolderAction::Prove {
                session: session.clone(),
                proof,
            }),
        )
        .await
        .unwrap();
    assert!(
        case.call(
            &parent,
            1,
            FileAction::Read {
                path: "child/source".into()
            }
        )
        .await
        .is_err()
    );
    let captured = Transactions::capture_root(&case.root(), "", 1, 1).unwrap();
    let fence = case.host.exclude(&captured).await.unwrap();
    drop(fence);
    assert!(
        case.host
            .request(
                case.root(),
                "paired/user:2".into(),
                request(FolderAction::File {
                    session,
                    sequence: 1,
                    action: FileAction::Read {
                        path: "source".into()
                    }
                })
            )
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(case.root().join("child/source")).unwrap(),
        "unchanged"
    );
}
