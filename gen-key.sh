export SERVER_NAME="$1"
export CLIENT_NAME="$2"
export EXTFILE='extfile.conf'

if [ -z "${CLIENT_NAME}" ]; then
	echo "usage: $0 <client-name> <server-name>" >&2
	exit 1
fi

if [ -z "${SERVER_NAME}" ]; then
	echo "usage: $0 <client-name> <server-name>" >&2
	exit 1
fi

echo 'subjectAltName = DNS:iridium' > "${EXTFILE}"

# server

## generate EC private key
openssl ecparam \
	-name prime256v1 \
	-genkey \
	-noout \
	-out "${SERVER_NAME}.pem"

## generate certificate signing request
openssl req \
	-new \
	-key "${SERVER_NAME}.pem" \
	-sha256 \
	-subj '/C=NL' \
	-out "${SERVER_NAME}.csr"

## generate CA certificate (server public key)
openssl x509 \
	-req \
	-in "${SERVER_NAME}.csr" \
	-extfile "${EXTFILE}" \
	-days 365 \
	-signkey "${SERVER_NAME}.pem" \
	-sha256 \
	-out "${SERVER_NAME}.pub.pem"


# client
## generate client private key
openssl ecparam \
	-name prime256v1 \
	-genkey \
	-noout \
	-out "${CLIENT_NAME}.pem"

## generate client csr (= public key)
openssl req \
	-key "${CLIENT_NAME}.pem" \
	-new -sha256 \
	-subj '/C=NL' \
	-out "${CLIENT_NAME}.csr"

## generate client certificate (=public key signed by CA)
openssl x509 \
	-req \
	-in "${CLIENT_NAME}.csr" \
	-extfile "${EXTFILE}" \
	-days 365 \
	-CA "${SERVER_NAME}.pub.pem" \
	-CAkey "${SERVER_NAME}.pem" \
	-set_serial '0xabcd' \
	-sha256 -out "${CLIENT_NAME}.pub.pem"

# cleanup
rm "${EXTFILE}" \
	"${SERVER_NAME}.csr" \
	"${CLIENT_NAME}.csr"
