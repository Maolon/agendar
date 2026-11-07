# Security

Please report vulnerabilities privately: open the repository's **Security** tab
and choose **Report a vulnerability**. Do not open a public issue.

Include what you found, how to reproduce it, and the version (Settings > About
in the app, or `agendar --version`). You will get a reply within a week.

Fixes ship in the latest release only.

Agendar keeps your data in a folder you choose (the Vault) and never sends it to
a server. Reports that matter most: a Vault file that runs code or reaches the
network, an agent command that escapes its project, and anything that writes
outside the Vault, the app's data folder or the harness settings you approved.
