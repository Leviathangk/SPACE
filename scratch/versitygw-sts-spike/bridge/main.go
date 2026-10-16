// Command bridge is a Kind-only STS shim prototype: it accepts the
// credential-manager authority's SigV4-signed AssumeRole, mints a
// short-lived OIDC token for the verified caller, exchanges it at
// versitygw's AssumeRoleWithWebIdentity with the same RoleArn, Policy and
// DurationSeconds, and answers in AssumeRole's response format. versitygw
// enforces role trust, the session policy and expiry. It also serves the
// issuer's discovery document and JWKS over HTTPS.
//
//	bridge keygen -dir D -hosts h1,h2      test CA, TLS leaf, signing keys
//	bridge serve [flags]                   STS on -listen, issuer on -issuer-listen
//	bridge mint [flags]                    print one token (test refusals)
package main

import (
	"flag"
	"fmt"
	"log"
	"net/http"
	"os"
	"strings"
	"time"
)

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: bridge keygen|serve|mint [flags]")
		os.Exit(2)
	}
	var err error
	switch os.Args[1] {
	case "keygen":
		fs := flag.NewFlagSet("keygen", flag.ExitOnError)
		dir := fs.String("dir", "", "output directory")
		hosts := fs.String("hosts", "localhost", "comma-separated TLS names and IPs")
		_ = fs.Parse(os.Args[2:])
		err = keygen(*dir, strings.Split(*hosts, ","))
	case "serve":
		err = serve(os.Args[2:])
	case "mint":
		err = mintCmd(os.Args[2:])
	default:
		err = fmt.Errorf("unknown command %q", os.Args[1])
	}
	if err != nil {
		log.Fatal(err)
	}
}

func serve(args []string) error {
	fs := flag.NewFlagSet("serve", flag.ExitOnError)
	listen := fs.String("listen", ":8080", "STS listen address (plain HTTP, as the authority's AWS_ENDPOINT_URL_STS)")
	issuerListen := fs.String("issuer-listen", ":8443", "issuer listen address (HTTPS)")
	tlsCert := fs.String("tls-cert", "", "issuer TLS chain, leaf first")
	tlsKey := fs.String("tls-key", "", "issuer TLS key")
	issuer := fs.String("issuer", "", "iss claim, as registered with versitygw")
	jwksBase := fs.String("jwks-base", "", "base URL for jwks_uri in the discovery document (default: -issuer)")
	keyPath := fs.String("signing-key", "", "PKCS#8 RSA signing key")
	audience := fs.String("audience", "", "aud claim")
	upstream := fs.String("upstream", "", "versitygw IAM service URL")
	ttl := fs.Duration("token-ttl", 5*time.Minute, "minted token lifetime")
	_ = fs.Parse(args)
	ids, err := parseIdentities(os.Getenv("BRIDGE_IDENTITIES"))
	if err != nil {
		return err
	}
	s, err := loadSigner(*keyPath)
	if err != nil {
		return err
	}
	if *issuer == "" || *audience == "" || *upstream == "" {
		return fmt.Errorf("-issuer, -audience and -upstream are required")
	}
	if *jwksBase == "" {
		*jwksBase = *issuer
	}
	b := &bridge{ids: ids, signer: s, issuer: *issuer, audience: *audience, tokenTTL: *ttl, upstream: *upstream,
		httpClient: &http.Client{Timeout: 15 * time.Second}}
	errs := make(chan error, 2)
	go func() {
		errs <- http.ListenAndServeTLS(*issuerListen, *tlsCert, *tlsKey, issuerHandler(*issuer, *jwksBase, s))
	}()
	go func() { errs <- http.ListenAndServe(*listen, b) }()
	log.Printf("bridge: sts on %s, issuer %s on %s (kid %s), %d identities, upstream %s", *listen, *issuer, *issuerListen, s.kid, len(ids), *upstream)
	return <-errs
}

// parseIdentities reads "ACCESS_KEY:SECRET:SUBJECT[,...]" from the
// environment, so secrets never appear in the process's argv.
func parseIdentities(spec string) (map[string]identity, error) {
	ids := map[string]identity{}
	for _, entry := range strings.Split(spec, ",") {
		if entry = strings.TrimSpace(entry); entry == "" {
			continue
		}
		parts := strings.SplitN(entry, ":", 3)
		if len(parts) != 3 || parts[0] == "" || parts[1] == "" || parts[2] == "" {
			return nil, fmt.Errorf("BRIDGE_IDENTITIES entry must be ACCESS_KEY:SECRET:SUBJECT")
		}
		ids[parts[0]] = identity{AccessKey: parts[0], Secret: parts[1], Subject: parts[2]}
	}
	if len(ids) == 0 {
		return nil, fmt.Errorf("BRIDGE_IDENTITIES is empty")
	}
	return ids, nil
}

func mintCmd(args []string) error {
	fs := flag.NewFlagSet("mint", flag.ExitOnError)
	keyPath := fs.String("signing-key", "", "PKCS#8 RSA signing key")
	iss := fs.String("iss", "", "iss claim")
	sub := fs.String("sub", "", "sub claim")
	aud := fs.String("aud", "", "aud claim")
	expIn := fs.Duration("exp-in", 5*time.Minute, "exp relative to now (negative: already expired)")
	kid := fs.String("kid", "", "override the kid header")
	alg := fs.String("alg", "RS256", "RS256 or none")
	_ = fs.Parse(args)
	s, err := loadSigner(*keyPath)
	if err != nil {
		return err
	}
	now := time.Now()
	claims := issuerClaims(*iss, *sub, *aud, now, *expIn)
	if *expIn < 0 {
		// An expired token was also issued in the past.
		claims["iat"] = now.Add(*expIn - time.Minute).Unix()
		claims["nbf"] = claims["iat"]
	}
	tok, err := s.mint(claims, *kid, *alg)
	if err != nil {
		return err
	}
	fmt.Println(tok)
	return nil
}
