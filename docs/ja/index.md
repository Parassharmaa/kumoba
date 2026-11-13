---
layout: home
title: kumoba
titleTemplate: AIエージェントのための高速サンドボックス

hero:
  name: kumoba
  text: AIエージェントのための高速サンドボックス
  tagline: 1秒未満で起動する本物のVMです。ノートPCでも、サーバーでも、クラウドでも動きます。
  image:
    src: /hero-terminal.svg
    alt: kumobaのrun、create、exec、desktopを実行しているターミナル
  actions:
    - theme: brand
      text: はじめに
      link: /ja/getting-started
    - theme: alt
      text: CLI(英語)
      link: /cli
    - theme: alt
      text: GitHub
      link: https://github.com/Parassharmaa/kumoba

features:
  - title: 高速
    details: 作成、起動、実行、削除までが約0.5秒です。
  - title: 本物のVM
    details: サンドボックスごとに独自のカーネルを持ちます。
  - title: エージェント向け
    details: CLIとSDKのすべてのコマンドにJSON Schemaがあります。
  - title: 安全なデフォルト
    details: 通信先の許可リスト、見えないシークレット、すべての接続の記録。
  - title: ライブデスクトップ
    details: ブラウザからデスクトップを見て操作できます。
  - title: どこでも動く
    details: ノートPC、サーバー、クラウド。Apache-2.0です。
---

<div class="quick-start">

## クイックスタート <span class="alpha-pill">alpha</span>

```sh
curl -fsSL https://raw.githubusercontent.com/Parassharmaa/kumoba/main/scripts/install.sh | bash
export PATH="$HOME/.kumoba/bin:$PATH"
kumobad &
kumoba run -- python3 -c 'print(21 * 2)'
```

macOSでは先にlibkrunをインストールしてください: `brew tap slp/krun && brew install slp/krun/libkrun slp/krun/libkrunfw zstd`。続きは[はじめに](/ja/getting-started)で説明しています。

</div>
