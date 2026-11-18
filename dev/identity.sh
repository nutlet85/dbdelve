#!/usr/bin/env bash
#
# Creates the self-signed identity dev/bundle.sh signs with. Run once.
#
# The Keychain pins a saved password's "always allow" to the signature of the
# app that asked for it. An ad-hoc signature is a hash of the binary, so every
# rebuild reads as a different app and prompts again. A certificate's signature
# does not move, so one answer holds for every build after it.
#
# Usage: dev/identity.sh
set -euo pipefail

NAME="DBDelve Dev Signing"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
# macOS refuses a PKCS#12 whose password is empty ("MAC verification failed
# during PKCS12 import"), so the throwaway bundle gets one. It guards a file
# that lives in a private temporary directory and is deleted on exit, never the
# identity itself, whose private key `security import` copies into the keychain.
P12_PASS="dbdelve-identity"

if security find-certificate -c "$NAME" "$KEYCHAIN" >/dev/null 2>&1; then
  echo "already have \"$NAME\""
  exit 0
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# A config file rather than -addext: the openssl on macOS is LibreSSL, and this
# spelling works on every version of it.
cat > "$WORK/openssl.cnf" <<CONF
[req]
distinguished_name = subject
x509_extensions = extensions
prompt = no

[subject]
CN = $NAME

[extensions]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
CONF

openssl req -x509 -newkey rsa:2048 -sha256 -days 7300 -nodes \
  -config "$WORK/openssl.cnf" -keyout "$WORK/key.pem" -out "$WORK/cert.pem"
# 3DES and a SHA-1 MAC rather than the PBES2/AES-256 that OpenSSL 3 exports by
# default: `security import` refuses that bundle with "MAC verification failed
# during PKCS12 import", and the openssl on PATH is not always macOS's LibreSSL,
# whose older defaults it could read.
openssl pkcs12 -export -out "$WORK/identity.p12" \
  -inkey "$WORK/key.pem" -in "$WORK/cert.pem" -name "$NAME" \
  -certpbe PBE-SHA1-3DES -keypbe PBE-SHA1-3DES -macalg sha1 \
  -passout "pass:$P12_PASS"

# -T codesign, so signing a rebuild does not ask for the private key each time.
security import "$WORK/identity.p12" -k "$KEYCHAIN" -P "$P12_PASS" -T /usr/bin/codesign
# Untrusted, the certificate is not an identity codesign will sign with. The
# user domain rather than -d, so this asks for the login password, not admin.
security add-trusted-cert -r trustRoot -p codeSign -k "$KEYCHAIN" "$WORK/cert.pem"

echo
echo "created \"$NAME\". dev/bundle.sh will pick it up on its own."
echo "The first launch after that still prompts once, because the saved"
echo "passwords were allowed to an ad-hoc signature -- answer Always Allow"
echo "and no later build will ask."
