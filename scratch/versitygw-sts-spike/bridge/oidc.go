package main

import (
	"crypto"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha1"
	"crypto/sha256"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"encoding/pem"
	"errors"
	"math/big"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"
)

var b64 = base64.RawURLEncoding

// signer holds the issuer's RSA key; kid is derived from the public key.
type signer struct {
	key *rsa.PrivateKey
	kid string
}

func loadSigner(path string) (*signer, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	block, _ := pem.Decode(raw)
	if block == nil {
		return nil, errors.New("no PEM block in " + path)
	}
	k, err := x509.ParsePKCS8PrivateKey(block.Bytes)
	if err != nil {
		return nil, err
	}
	rk, ok := k.(*rsa.PrivateKey)
	if !ok {
		return nil, errors.New("signing key is not RSA")
	}
	der, _ := x509.MarshalPKIXPublicKey(&rk.PublicKey)
	sum := sha256.Sum256(der)
	return &signer{key: rk, kid: hex.EncodeToString(sum[:8])}, nil
}

// mint signs claims as an RS256 JWT. alg "none" produces an unsigned token
// (for refusal checks only).
func (s *signer) mint(claims map[string]any, kid, alg string) (string, error) {
	if kid == "" {
		kid = s.kid
	}
	header := map[string]any{"alg": "RS256", "typ": "JWT", "kid": kid}
	if alg == "none" {
		header = map[string]any{"alg": "none", "typ": "JWT"}
	}
	h, _ := json.Marshal(header)
	c, err := json.Marshal(claims)
	if err != nil {
		return "", err
	}
	input := b64.EncodeToString(h) + "." + b64.EncodeToString(c)
	if alg == "none" {
		return input + ".", nil
	}
	digest := sha256.Sum256([]byte(input))
	sig, err := rsa.SignPKCS1v15(rand.Reader, s.key, crypto.SHA256, digest[:])
	if err != nil {
		return "", err
	}
	return input + "." + b64.EncodeToString(sig), nil
}

// issuerClaims are what the bridge mints for a verified caller: short-lived,
// one subject, one audience.
func issuerClaims(iss, sub, aud string, now time.Time, ttl time.Duration) map[string]any {
	nonce := make([]byte, 12)
	_, _ = rand.Read(nonce)
	return map[string]any{
		"iss": iss,
		"sub": sub,
		"aud": aud,
		"iat": now.Unix(),
		"nbf": now.Unix(),
		"exp": now.Add(ttl).Unix(),
		"jti": hex.EncodeToString(nonce),
	}
}

// issuerHandler serves the OIDC discovery document and the JWKS: all
// versitygw fetches from an issuer.
func issuerHandler(iss, jwksBase string, s *signer) http.Handler {
	jwks, _ := json.Marshal(map[string]any{"keys": []any{map[string]string{
		"kty": "RSA", "use": "sig", "alg": "RS256", "kid": s.kid,
		"n": b64.EncodeToString(s.key.N.Bytes()),
		"e": b64.EncodeToString(big.NewInt(int64(s.key.E)).Bytes()),
	}}})
	discovery, _ := json.Marshal(map[string]any{
		"issuer":                                iss,
		"jwks_uri":                              strings.TrimRight(jwksBase, "/") + "/jwks.json",
		"response_types_supported":              []string{"id_token"},
		"subject_types_supported":               []string{"public"},
		"id_token_signing_alg_values_supported": []string{"RS256"},
	})
	mux := http.NewServeMux()
	serve := func(body []byte) http.HandlerFunc {
		return func(w http.ResponseWriter, _ *http.Request) {
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write(body)
		}
	}
	mux.Handle("GET /.well-known/openid-configuration", serve(discovery))
	mux.Handle("GET /jwks.json", serve(jwks))
	return mux
}

// keygen writes a CA, a TLS leaf for hosts (chain in tls.pem), the trusted
// signing key and an untrusted one, and the CA's SHA-1 thumbprint, which is
// what versitygw pins: the top certificate of the presented chain.
func keygen(dir string, hosts []string) error {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	caKey, _ := rsa.GenerateKey(rand.Reader, 2048)
	caTmpl := &x509.Certificate{
		SerialNumber: big.NewInt(1), Subject: pkix.Name{CommonName: "versitygw-sts-spike test CA"},
		NotBefore: time.Now().Add(-time.Hour), NotAfter: time.Now().Add(30 * 24 * time.Hour),
		IsCA: true, BasicConstraintsValid: true, KeyUsage: x509.KeyUsageCertSign | x509.KeyUsageDigitalSignature,
	}
	caDER, err := x509.CreateCertificate(rand.Reader, caTmpl, caTmpl, &caKey.PublicKey, caKey)
	if err != nil {
		return err
	}
	ca, _ := x509.ParseCertificate(caDER)
	leafKey, _ := rsa.GenerateKey(rand.Reader, 2048)
	leafTmpl := &x509.Certificate{
		SerialNumber: big.NewInt(2), Subject: pkix.Name{CommonName: hosts[0]},
		NotBefore: time.Now().Add(-time.Hour), NotAfter: time.Now().Add(30 * 24 * time.Hour),
		KeyUsage: x509.KeyUsageDigitalSignature | x509.KeyUsageKeyEncipherment, ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
	}
	for _, h := range hosts {
		if ip := net.ParseIP(h); ip != nil {
			leafTmpl.IPAddresses = append(leafTmpl.IPAddresses, ip)
		} else {
			leafTmpl.DNSNames = append(leafTmpl.DNSNames, h)
		}
	}
	leafDER, err := x509.CreateCertificate(rand.Reader, leafTmpl, ca, &leafKey.PublicKey, caKey)
	if err != nil {
		return err
	}
	chain := append(pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: leafDER}), pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: caDER})...)
	thumb := sha1.Sum(caDER)
	files := map[string][]byte{
		"tls.pem":     chain,
		"tls.key":     pemKey(leafKey),
		"ca.pem":      pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: caDER}),
		"ca.sha1":     []byte(hex.EncodeToString(thumb[:]) + "\n"),
		"signing.key": nil,
		"other.key":   nil,
	}
	for _, name := range []string{"signing.key", "other.key"} {
		k, _ := rsa.GenerateKey(rand.Reader, 2048)
		files[name] = pemKey(k)
	}
	for name, data := range files {
		if err := os.WriteFile(filepath.Join(dir, name), data, 0o644); err != nil {
			return err
		}
	}
	return nil
}

func pemKey(k *rsa.PrivateKey) []byte {
	der, _ := x509.MarshalPKCS8PrivateKey(k)
	return pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: der})
}
