# Release signing key

The release workflow and installer require a dedicated OpenPGP signing key.
The public key is intentionally not present yet. Before the first release, a
maintainer must add its ASCII-armored public half at:

```text
keys/signing-key.asc
```

Never commit the private key, its passphrase, or an exported keyring. Export
the matching unencrypted private key as ASCII armor and save the complete
value in the GitHub Actions repository secret `GPG_PRIVATE_KEY`.

The release key should be used only to sign `dof` checksum manifests. Record
and independently verify its full fingerprint before committing the public
key. A typical setup is:

```sh
gpg --batch --pinentry-mode loopback --passphrase '' \
  --quick-generate-key "dof release signing" ed25519 sign 2y
gpg --list-secret-keys --keyid-format long
gpg --armor --export <FULL_FINGERPRINT> > keys/signing-key.asc
gpg --armor --export-secret-keys <FULL_FINGERPRINT>
```

The private key supplied to Actions must be usable non-interactively. Use a
dedicated key without a passphrase, because the release job does not store or
request one. Protect access to the repository secret accordingly.

After committing the public key, configure the secret in the repository and
follow [`release-procedure.md`](../release-procedure.md). Until both halves
are configured, release creation and installation intentionally fail closed.
