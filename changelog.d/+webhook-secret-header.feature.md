Moved the webhook secret from `Authorization: Bearer` to the `X-Svab-Bot-Secret` header, so the bot can run behind an ingress that uses `Authorization` for its own tokens.
