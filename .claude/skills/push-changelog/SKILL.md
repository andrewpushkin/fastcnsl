---
name: push-changelog
description: Pull, stamp the Unreleased section of CHANGELOG.md with the version this push will have, commit that and push the current branch. Run only when the user types /push-changelog.
disable-model-invocation: true
---

# Push and update the changelog

Typing `/push-changelog` is the user's approval for this one push. That is why
the skill can only be started by the user. Stop at the first step that fails
and say why: a half-done run is easy to finish by hand, a forced one is not.

Run everything from the repository root, in this order.

## 1. Pull

```sh
git pull --ff-only
```

Only fast-forward. A merge commit would be an extra commit nobody asked for,
and it changes the version. If the branches have diverged, stop and ask the
user how to reconcile them.

## 2. Work out the version

The version is `0.1.<commits>` (`scripts/version.sh`). The changelog commit made
in step 4 is one more commit, so the version of this push is the count + 1.
Count after the pull, because the pull can add commits.

```sh
current=$(scripts/version.sh)
version="${current%.*}.$(( ${current##*.} + 1 ))"
```

CI numbers releases by the commits on `main`, so the number only matches if
this branch contains all of `main`. Check it, and stop if it doesn't:

```sh
git merge-base --is-ancestor origin/main HEAD
```

## 3. Stamp the changelog

`CHANGELOG.md` must have a `## Unreleased` section with at least one entry
under it. If it has none, stop: there is nothing to release, and the user may
have forgotten to write the entries. Don't invent any.

```sh
sed -i "s/^## Unreleased$/## $version ($(date +%F))/" CHANGELOG.md
```

Don't add a new empty `## Unreleased` section; the next change adds one.

## 4. Commit

Commit only the changelog, as exactly one commit. Other uncommitted work stays
out of it, and any second commit would make the version wrong. End the message
with the attribution lines the session asks for.

```sh
git add CHANGELOG.md
git commit -m "Changelog: $version"
```

## 5. Push

Push the current branch and nothing else. Never force. Pushing to `main`
publishes a release, so never push `main` from a different branch here.

```sh
git push origin HEAD
```

## 6. Report

Say which version was stamped, which branch was pushed and how many commits
went with it. Mention any uncommitted files that were left out.
