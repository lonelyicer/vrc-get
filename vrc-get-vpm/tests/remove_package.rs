use crate::common::VirtualProjectBuilder;
use futures::executor::block_on;
use vrc_get_vpm::unity_project::pending_project_changes::RemoveReason;
use vrc_get_vpm::version::Version;

mod common;

#[test]
fn basic_remove() {
    block_on(async {
        let project = VirtualProjectBuilder::new()
            .add_dependency("com.anatawa12.gists", Version::new(1, 0, 0))
            .add_locked("com.anatawa12.gists", Version::new(1, 0, 0), &[])
            .build()
            .await
            .unwrap();

        let result = project
            .remove_request(&[&"com.anatawa12.gists"])
            .await
            .unwrap();

        assert_eq!(result.package_changes().len(), 1);
        assert_eq!(result.remove_legacy_folders().len(), 0);
        assert_eq!(result.remove_legacy_files().len(), 0);
        assert_eq!(result.conflicts().len(), 0);

        let gists_change = result.package_changes().get("com.anatawa12.gists").unwrap();
        let gists_change = gists_change.as_remove().expect("gists is not removing");
        assert_eq!(gists_change.reason(), RemoveReason::Requested);
    })
}

#[test]
fn transitive_unused_remove() {
    block_on(async {
        // create minimum project
        let project = VirtualProjectBuilder::new()
            .add_dependency("com.vrchat.avatars", Version::new(1, 0, 0))
            .add_locked(
                "com.vrchat.avatars",
                Version::new(1, 0, 0),
                &[("com.vrchat.base", "1.0.0")],
            )
            .add_locked("com.vrchat.base", Version::new(1, 0, 0), &[])
            .build()
            .await
            .unwrap();

        let result = project
            .remove_request(&[&"com.vrchat.avatars"])
            .await
            .unwrap();

        assert_eq!(result.package_changes().len(), 2);
        assert_eq!(result.remove_legacy_folders().len(), 0);
        assert_eq!(result.remove_legacy_files().len(), 0);
        assert_eq!(result.conflicts().len(), 0);

        let avatars_change = result.package_changes().get("com.vrchat.avatars").unwrap();
        let avatars_change = avatars_change.as_remove().expect("avatars is not removing");
        assert_eq!(avatars_change.reason(), RemoveReason::Requested);

        let avatars_change = result.package_changes().get("com.vrchat.base").unwrap();
        let avatars_change = avatars_change.as_remove().expect("base is not removing");
        assert_eq!(avatars_change.reason(), RemoveReason::Unused);
    })
}

#[test]
fn do_not_remove_transitively_when_untouched() {
    block_on(async {
        // create minimum project
        let project = VirtualProjectBuilder::new()
            .add_dependency("com.vrchat.avatars", Version::new(1, 0, 0))
            .add_locked(
                "com.vrchat.avatars",
                Version::new(1, 0, 0),
                &[("com.vrchat.base", "1.0.0")],
            )
            .add_locked("com.vrchat.base", Version::new(1, 0, 0), &[])
            .add_locked(
                "com.anatawa12.untouched_library",
                Version::new(1, 0, 0),
                &[],
            )
            .build()
            .await
            .unwrap();

        let result = project
            .remove_request(&[&"com.vrchat.avatars"])
            .await
            .unwrap();

        assert_eq!(result.package_changes().len(), 2);
        assert_eq!(result.remove_legacy_folders().len(), 0);
        assert_eq!(result.remove_legacy_files().len(), 0);
        assert_eq!(result.conflicts().len(), 0);

        let avatars_change = result.package_changes().get("com.vrchat.avatars").unwrap();
        let avatars_change = avatars_change.as_remove().expect("avatars is not removing");
        assert_eq!(avatars_change.reason(), RemoveReason::Requested);

        let avatars_change = result.package_changes().get("com.vrchat.base").unwrap();
        let avatars_change = avatars_change.as_remove().expect("base is not removing");
        assert_eq!(avatars_change.reason(), RemoveReason::Unused);
    })
}
