use super::*;

fn page(namespaces: &[&str], cgroups: &[&str], last: bool) -> ListOrphanCandidatesResponse {
    ListOrphanCandidatesResponse {
        orphaned_namespaces: namespaces.iter().map(ToString::to_string).collect(),
        orphaned_cgroups: cgroups.iter().map(ToString::to_string).collect(),
        next_page_token: if last { String::new() } else { "walk:1".into() },
        complete: last,
        ..Default::default()
    }
}

fn walk(observer: &mut Observer, pages: Vec<ListOrphanCandidatesResponse>) -> WalkSummary {
    let mut summary = None;
    for page in pages {
        assert!(summary.is_none(), "a walk ends with its last page");
        summary = observer.record_page(page);
    }
    summary.expect("the last page ends the walk")
}

/// A walk's pages make one summary. A candidate is reported as needing
/// inspection once, in the first complete walk after the one that first found
/// it: a single sighting can be a sandbox created or torn down mid-walk.
#[test]
fn walk_reports_a_candidate_once_it_outlasts_a_walk() {
    let mut observer = Observer::default();
    let cgroup = "/sys/fs/cgroup/space/sbx-00000000-0000-4000-8000-000000000001";

    let first = walk(
        &mut observer,
        vec![page(&["sns-7"], &[], false), page(&[], &[cgroup], true)],
    );
    let second = walk(
        &mut observer,
        vec![page(&["sns-7"], &[], false), page(&[], &[cgroup], true)],
    );
    let third = walk(&mut observer, vec![page(&["sns-7"], &[cgroup], true)]);
    let fourth = walk(&mut observer, vec![page(&[], &[], true)]);
    let fifth = walk(&mut observer, vec![page(&["sns-7"], &[], true)]);
    let sixth = walk(&mut observer, vec![page(&["sns-7"], &[], true)]);

    assert_eq!(
        (first.verdict, first.candidates, first.new_persistent),
        (Verdict::Known, 2, 0)
    );
    assert_eq!(
        (second.verdict, second.persistent, second.new_persistent),
        (Verdict::NewlyPersistent, 2, 2)
    );
    assert_eq!(
        (third.verdict, third.persistent, third.new_persistent),
        (Verdict::Known, 2, 0)
    );
    assert_eq!(fourth.verdict, Verdict::Clean);
    assert_eq!(
        fifth.verdict,
        Verdict::Known,
        "gone and back is a new sighting"
    );
    assert_eq!(sixth.verdict, Verdict::NewlyPersistent);
    assert_eq!(sixth.samples, ["sns-7"]);
}

/// A walk that could not read every source is reported whatever it found, and
/// does not count as a sighting either way: a candidate it missed may still
/// be there.
#[test]
fn incomplete_walk_is_reported_and_does_not_change_persistence() {
    let mut observer = Observer::default();
    let mut unreadable = page(&[], &[], true);
    unreadable.complete = false;
    unreadable.warnings = vec!["Cgroups: permission denied".into()];

    let first = walk(&mut observer, vec![page(&["sns-7"], &[], true)]);
    let incomplete = walk(&mut observer, vec![unreadable]);
    let third = walk(&mut observer, vec![page(&["sns-7"], &[], true)]);

    assert_eq!(first.verdict, Verdict::Known);
    assert_eq!(incomplete.verdict, Verdict::Incomplete);
    assert_eq!(incomplete.warnings, ["Cgroups: permission denied"]);
    assert_eq!(third.verdict, Verdict::NewlyPersistent);
}

/// Memory stays bounded however many candidates a node has; a walk with more
/// than are tracked is reported as incomplete.
#[test]
fn walk_tracks_a_bounded_number_of_candidates() {
    let mut observer = Observer::default();
    let names: Vec<String> = (0..MAX_TRACKED + 5).map(|i| format!("sns-{i}")).collect();
    let mut pages: Vec<_> = names
        .chunks(256)
        .map(|chunk| {
            let chunk: Vec<&str> = chunk.iter().map(String::as_str).collect();
            page(&chunk, &[], false)
        })
        .collect();
    pages.push(page(&[], &[], true));

    let summary = walk(&mut observer, pages);

    assert_eq!(summary.verdict, Verdict::Incomplete);
    assert_eq!(summary.candidates, MAX_TRACKED + 5);
    assert_eq!(summary.samples.len(), MAX_SAMPLES);
    assert_eq!(observer.previous.len(), 0);
}
