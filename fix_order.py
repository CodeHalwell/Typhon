import sys

prefixes = ["DB", "API", "APP", "CLIENT", "JWT", "SECRET", "ACCESS", "AUTH", "BEARER", "CSRF"]

# Current existing list
current = [
    "PASSPHRASE", "AUTHORIZATION", "CREDENTIALS", "CREDENTIAL", "WEBHOOK",
    "SIGNING", "COOKIE", "DB_PASSWORD", "DBPASSWORD", "DB_PASS", "DBPASS",
    "DB_PWD", "DBPWD", "API_PASSWORD", "APIPASSWORD", "DB_SECRET", "DBSECRET",
    "API_SECRET", "APISECRET", "APP_SECRET", "APPSECRET", "CLIENT_SECRET",
    "CLIENTSECRET", "JWT_SECRET", "JWTSECRET", "SECRET_KEY", "SECRETKEY",
    "ACCESS_TOKEN", "ACCESSTOKEN", "AUTH_TOKEN", "AUTHTOKEN", "BEARER_TOKEN",
    "BEARERTOKEN", "CSRF_TOKEN", "CSRFTOKEN", "JWT_TOKEN", "JWTTOKEN",
    "API_TOKEN", "APITOKEN", "PASSWORD", "SECRET", "TOKEN", "PRIVATE_KEY",
    "PRIVATEKEY", "PUBLIC_KEY", "PUBLICKEY", "SSH_KEY", "SSHKEY", "API_KEY",
    "APIKEY", "PRIVKEY", "KEY", "PWD", "PASS", "DSN",
]

# Build new list
new_keywords = set()

for prefix in prefixes:
    for base in ["PASSWORD", "PWD", "PASS", "SECRET", "TOKEN", "KEY"]:
        new_keywords.add(f"{prefix}_{base}")
        new_keywords.add(f"{prefix}{base}")

new_keywords.update(current)

# Sort longest first, then alphabetically
sorted_keywords = sorted(list(new_keywords), key=lambda x: (-len(x), x))

for kw in ["PASSPHRASE", "AUTHORIZATION", "CREDENTIALS", "CREDENTIAL", "WEBHOOK", "SIGNING", "COOKIE"]:
    sorted_keywords.remove(kw)

final_list = ["PASSPHRASE", "AUTHORIZATION", "CREDENTIALS", "CREDENTIAL", "WEBHOOK", "SIGNING", "COOKIE"] + sorted_keywords

print("pub const SECRET_NAME_KEYWORDS: &[&str] = &[")
for kw in final_list:
    print(f'    "{kw}",')
print("];")
