import { defineConfig } from "vitepress";

const repo = "https://github.com/Parassharmaa/kumoba";
const repoFiles: Record<string, string> = {
  LICENSE: `${repo}/blob/main/LICENSE`,
  NOTICE: `${repo}/blob/main/NOTICE`,
  "CHANGELOG.md": "/changelog",
  "CONTRIBUTING.md": "/contributing",
  "SECURITY.md": "/security",
  "sdk/typescript": `${repo}/tree/main/sdk/typescript`,
  "crates/kumoba/examples/hello.rs": `${repo}/blob/main/crates/kumoba/examples/hello.rs`,
};

function siteLink(href: string): string {
  if (/^[a-z]+:/i.test(href) || href.startsWith("#") || href.startsWith("/")) return href;
  const [path, hash] = href.split("#");
  const target =
    repoFiles[path] ?? (path.startsWith("docs/") ? `/${path.slice(5).replace(/\.md$/, "")}` : href);
  return hash ? `${target}#${hash}` : target;
}

export default defineConfig({
  title: "kumoba",
  description: "Fast sandboxes for AI agents: real VMs that boot in under a second",
  base: "/kumoba/",
  cleanUrls: true,
  lastUpdated: true,
  head: [["link", { rel: "icon", href: "/kumoba/favicon.svg" }]],
  srcExclude: ["README.md"],
  markdown: {
    config(md) {
      md.core.ruler.push("kumoba-links", (state) => {
        for (const token of state.tokens) {
          for (const child of token.children ?? []) {
            if (child.type === "link_open") {
              const href = child.attrGet("href");
              if (href) child.attrSet("href", siteLink(href));
            }
          }
        }
      });
    },
  },
  locales: {
    root: {
      label: "English",
      lang: "en",
      themeConfig: {
        nav: [
          { text: "Guide", link: "/getting-started" },
          { text: "CLI", link: "/cli" },
          { text: "HTTP API", link: "/http-api" },
          {
            text: "v0.2 · alpha",
            items: [
              { text: "Changelog", link: "/changelog" },
              { text: "Releases", link: `${repo}/releases` },
              { text: "npm", link: "https://www.npmjs.com/package/kumoba" },
              { text: "crates.io", link: "https://crates.io/crates/kumobad" },
            ],
          },
        ],
        sidebar: [
          {
            text: "Tutorial",
            items: [
              { text: "Getting started", link: "/getting-started" },
              { text: "Install options", link: "/install" },
            ],
          },
          {
            text: "How-to guides",
            items: [
              { text: "Run code once", link: "/guide/run-code" },
              { text: "Work in a sandbox", link: "/guide/sandboxes" },
              { text: "Open the desktop live view", link: "/guide/desktop" },
              { text: "Open a web app", link: "/guide/expose" },
              { text: "Control network access", link: "/guide/network" },
              { text: "Use a secret token", link: "/guide/secrets" },
              { text: "Run kumobad as a service", link: "/service" },
              { text: "Upgrade or uninstall", link: "/guide/upgrade" },
              { text: "Troubleshooting", link: "/troubleshooting" },
            ],
          },
          {
            text: "Reference",
            items: [
              { text: "CLI", link: "/cli" },
              { text: "TypeScript SDK", link: "/typescript" },
              { text: "Rust", link: "/rust" },
              { text: "HTTP API", link: "/http-api" },
              { text: "Configuration", link: "/configuration" },
              { text: "Images", link: "/images" },
              { text: "Changelog", link: "/changelog" },
            ],
          },
          {
            text: "Explanation",
            items: [
              { text: "How it works", link: "/architecture" },
              { text: "How secrets work", link: "/secrets" },
              { text: "Security model", link: "/security" },
            ],
          },
          {
            text: "Project",
            items: [
              { text: "Contributing", link: "/contributing" },
              { text: "Releasing", link: "/releasing" },
            ],
          },
        ],
        editLink: { pattern: `${repo}/edit/main/docs/:path`, text: "Edit this page on GitHub" },
        footer: {
          message: "Alpha software: APIs, file formats and images may change between releases. Apache-2.0.",
          copyright: "kumoba contributors",
        },
      },
    },
    ja: {
      label: "日本語",
      lang: "ja",
      link: "/ja/",
      description: "AIエージェントのための高速サンドボックス",
      themeConfig: {
        nav: [
          { text: "ガイド", link: "/ja/getting-started" },
          { text: "CLI(英語)", link: "/cli" },
          {
            text: "v0.2 · アルファ版",
            items: [
              { text: "変更履歴(英語)", link: "/changelog" },
              { text: "リリース", link: `${repo}/releases` },
              { text: "npm", link: "https://www.npmjs.com/package/kumoba" },
              { text: "crates.io", link: "https://crates.io/crates/kumobad" },
            ],
          },
        ],
        sidebar: [
          {
            text: "チュートリアル",
            items: [{ text: "はじめに", link: "/ja/getting-started" }],
          },
          {
            text: "ハウツー",
            items: [
              { text: "コードを1回だけ実行する", link: "/ja/guide/run-code" },
              { text: "デスクトップのライブビューを開く", link: "/ja/guide/desktop" },
              { text: "トラブルシューティング", link: "/ja/troubleshooting" },
            ],
          },
          {
            text: "英語のドキュメント",
            items: [
              { text: "サンドボックスで作業する", link: "/guide/sandboxes" },
              { text: "Webアプリを開く", link: "/guide/expose" },
              { text: "ネットワークを制御する", link: "/guide/network" },
              { text: "シークレットを使う", link: "/guide/secrets" },
              { text: "CLIリファレンス", link: "/cli" },
              { text: "HTTP API", link: "/http-api" },
              { text: "設定", link: "/configuration" },
              { text: "仕組み", link: "/architecture" },
              { text: "セキュリティモデル", link: "/security" },
            ],
          },
        ],
        editLink: { pattern: `${repo}/edit/main/docs/:path`, text: "GitHubでこのページを編集" },
        docFooter: { prev: "前のページ", next: "次のページ" },
        outline: { label: "目次" },
        lastUpdated: { text: "最終更新" },
        returnToTopLabel: "トップに戻る",
        sidebarMenuLabel: "メニュー",
        darkModeSwitchLabel: "表示モード",
        langMenuLabel: "言語",
        footer: {
          message: "アルファ版です。API、ファイル形式、イメージはリリース間で変わる可能性があります。Apache-2.0。",
          copyright: "kumoba contributors",
        },
      },
    },
  },
  themeConfig: {
    logo: { src: "/favicon.svg", width: 22, height: 22 },
    socialLinks: [{ icon: "github", link: repo }],
    search: {
      provider: "local",
      options: {
        locales: {
          ja: { translations: { button: { buttonText: "検索", buttonAriaLabel: "検索" } } },
        },
      },
    },
  },
});
