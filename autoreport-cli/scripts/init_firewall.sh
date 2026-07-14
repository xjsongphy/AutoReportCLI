#!/usr/bin/env bash
# Container-network helper adapted from Codex's init_firewall.sh.
set -euo pipefail

domains_file=/etc/autoreport/allowed_domains.txt
mapfile -t domains < <(test -f "$domains_file" && sed '/^\s*#/d;/^\s*$/d' "$domains_file" || printf '%s\n' api.openai.com api.anthropic.com)
test "${#domains[@]}" -gt 0 || { echo "no allowed domains" >&2; exit 1; }

iptables -F; iptables -X; ipset destroy autoreport-allowed 2>/dev/null || true
ipset create autoreport-allowed hash:net
for domain in "${domains[@]}"; do
  while read -r ip; do test -n "$ip" && ipset add autoreport-allowed "$ip"; done < <(dig +short A "$domain")
done
iptables -A INPUT -i lo -j ACCEPT; iptables -A OUTPUT -o lo -j ACCEPT
iptables -A OUTPUT -p udp --dport 53 -j ACCEPT; iptables -A INPUT -p udp --sport 53 -j ACCEPT
iptables -A INPUT -m state --state ESTABLISHED,RELATED -j ACCEPT
iptables -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT
iptables -A OUTPUT -m set --match-set autoreport-allowed dst -j ACCEPT
iptables -P INPUT DROP; iptables -P FORWARD DROP; iptables -P OUTPUT DROP
echo "AutoReport firewall configured for: ${domains[*]}"
