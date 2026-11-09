---
name: push-main
description: Update CHANGELOG.md, stamp its Unreleased section with the version the release will have, commit that and push the current code to main. Use only when the user's current message explicitly asks to push to main.
---

# Push to main

A push to `main` publishes a release, numbered by CI as
`0.1.<commits on main>`. The changelog has to name that number before the push
goes up, so this skill works it out in advance.

The user's request covers this one push. Stop at the first step that fails and
say why: a half-done run is easy to finish by hand, a forced one is not.

Run everything from the repository root, in this order.

## 1. Check it will fast-forward

```sh
git fetch origin
git merge-base --is-ancestor origin/main HEAD
```

If that fails, `main` has commits this branch lacks: the push would be refused
and the version would be wrong. Stop and ask the user how to reconcile them.

## 2. Update the changelog

Run the `update-changelog` skill, which commits `CHANGELOG.md` if anything was
missing. It comes before step 3 because its commit counts towards the version.

## 3. Work out the version

The stamp in step 4 is one more commit, so the version of this push is what
`--next` prints. `HEAD` holds everything that will be on `main`, because step 1 passed.

```sh
version=$(scripts/get_version.sh --next)
```

## 4. Stamp and commit

If `CHANGELOG.md` has no `## Unreleased` section with an entry under it, there
is nothing to stamp: skip to step 5, and say so in the report. Pushing this way
adds no commit, so the release is numbered what `scripts/get_version.sh` prints.

Otherwise stamp it, and commit only the changelog, as exactly one commit. Other
uncommitted work stays out of it, and any second commit would make the version
wrong. End the message with the attribution lines the session asks for.

```sh
sed -i "s/^## Unreleased$/## $version ($(date +%F))/" CHANGELOG.md
git add CHANGELOG.md
git commit -m "Changelog: $version"
```

Don't add a new empty `## Unreleased` section; the next change adds one.

## 5. Push

Never force.

```sh
git push origin HEAD:main
```

## 6. Report

Say which version was released, how many commits went to `main`, and whether
the current branch's own remote is now behind. Mention any uncommitted files
that were left out.
