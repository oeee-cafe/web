#!/bin/sh
# Regenerates the certificate chains app_store.rs's tests sign with: the
# shape of Apple's x5c chain -- a P-384 root, an intermediate that carries
# Apple's WWDR marker (1.2.840.113635.100.6.2.1) and a leaf that carries its
# receipt-signing marker (1.2.840.113635.100.6.11.1), each a non-critical
# extension holding NULL, as in Apple's own -- under a root of our own. The
# leaf's key is ../app_store_test_key.p8, the key the fake App Store signs
# with.
#
# Beside the good chain, one with each marker missing, which the site must
# refuse, and a key that is not the leaf's. Valid for a century, so the
# tests do not start failing on a date.
#
# The real Apple intermediate and leaf (apple_*.der) are not made here: they
# are copied from Apple's app-store-server-library-python tests.
set -eu
cd "$(dirname "$0")"
OPENSSL=${OPENSSL:-openssl}
DAYS=36500
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

cat >"$tmp/ext.cnf" <<'EOF'
[intermediate]
basicConstraints = critical, CA:TRUE, pathlen:0
keyUsage = critical, keyCertSign, cRLSign
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid
1.2.840.113635.100.6.2.1 = ASN1:NULL

[intermediate_unmarked]
basicConstraints = critical, CA:TRUE, pathlen:0
keyUsage = critical, keyCertSign, cRLSign
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid

[leaf]
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature
authorityKeyIdentifier = keyid
1.2.840.113635.100.6.11.1 = ASN1:NULL

[leaf_unmarked]
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature
authorityKeyIdentifier = keyid
EOF

key() { "$OPENSSL" genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-384 -out "$1"; }

# issue <subject> <key> <issuer cert> <issuer key> <section> <out>
issue() {
    "$OPENSSL" req -new -key "$2" -subj "$1" -out "$tmp/req.csr"
    "$OPENSSL" x509 -req -in "$tmp/req.csr" -CA "$3" -CAkey "$4" \
        -set_serial "0x$("$OPENSSL" rand -hex 8)" -days $DAYS -sha384 \
        -extfile "$tmp/ext.cnf" -extensions "$5" -outform DER -out "$6"
}

key "$tmp/root.key"
"$OPENSSL" req -new -x509 -key "$tmp/root.key" -subj "/CN=Test Root CA/O=Oeee Cafe Tests" \
    -days $DAYS -sha384 -addext "basicConstraints=critical,CA:TRUE" \
    -addext "keyUsage=critical,keyCertSign,cRLSign" -out "$tmp/root.pem"
"$OPENSSL" x509 -in "$tmp/root.pem" -outform DER -out root.der

key "$tmp/int.key"
issue "/CN=Test WWDR CA/O=Oeee Cafe Tests" "$tmp/int.key" "$tmp/root.pem" "$tmp/root.key" intermediate intermediate.der
issue "/CN=Test WWDR CA without the marker/O=Oeee Cafe Tests" "$tmp/int.key" "$tmp/root.pem" "$tmp/root.key" intermediate_unmarked intermediate_unmarked.der
"$OPENSSL" x509 -inform DER -in intermediate.der -out "$tmp/int.pem"
"$OPENSSL" x509 -inform DER -in intermediate_unmarked.der -out "$tmp/int_unmarked.pem"

issue "/CN=Test Receipt Signing/O=Oeee Cafe Tests" ../app_store_test_key.p8 "$tmp/int.pem" "$tmp/int.key" leaf leaf.der
issue "/CN=Test Receipt Signing without the marker/O=Oeee Cafe Tests" ../app_store_test_key.p8 "$tmp/int.pem" "$tmp/int.key" leaf_unmarked leaf_unmarked.der
issue "/CN=Test Receipt Signing under the unmarked CA/O=Oeee Cafe Tests" ../app_store_test_key.p8 "$tmp/int_unmarked.pem" "$tmp/int.key" leaf leaf_under_unmarked.der

# Some other P-256 key: a transaction signed with it and sent with the good
# chain claims a signer whose certificate says otherwise.
"$OPENSSL" genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out impostor_key.p8
