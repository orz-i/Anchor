export function validateRedirectUris(raw: string) {
  for (const value of raw.split(/\r?\n/).map((item) => item.trim()).filter(Boolean)) {
    if (value.includes("*")) throw new Error(`OAuth Callback URL 不允许通配符：${value}`);
    let parsed: URL;
    try { parsed = new URL(value); } catch { throw new Error(`OAuth Callback URL 无效：${value}`); }
    const loopback = ["localhost", "127.0.0.1", "[::1]", "::1"].includes(parsed.hostname);
    if (parsed.protocol !== "https:" && !(parsed.protocol === "http:" && loopback)) throw new Error(`OAuth Callback URL 必须使用 HTTPS 或 HTTP loopback：${value}`);
    if (parsed.hash) throw new Error(`OAuth Callback URL 不能包含 fragment：${value}`);
  }
}

export function validateRedirectHosts(raw: string) {
  for (const value of raw.split(/\r?\n/).map((item) => item.trim()).filter(Boolean)) {
    if (!/^(\*\.)?[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$/i.test(value) || value.includes("..") || value.includes(":") || value.includes("/") || value.includes("?")) throw new Error(`Callback 域名格式无效：${value}`);
  }
}
