---
layout: home
title: kumoba
titleTemplate: AIエージェントのためのローカルmicroVMサンドボックス

hero:
  name: kumoba
  text: AIエージェントのためのローカルmicroVMサンドボックス
  tagline: 独自カーネルで動く本物のVM、設定不要のデーモン、CLIとSDK。セルフホスト、オープンソース、アルファ版です。
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
  - title: コンテナではなく本物のVM
    details: サンドボックスはそれぞれ独自のカーネルを持つlibkrunのmicroVMです。LinuxではKVM、Apple siliconではHypervisor.frameworkを使い、起動から実行、削除まで1秒未満です。
  - title: コマンド1つで始められる
    details: インストーラーがデーモン、CLI、すぐに使えるイメージを取得します。kumobadは設定不要で、CLIとSDKは自動でデーモンを見つけます。
  - title: エージェント向けの設計
    details: CLIはデーモンのカタログからコマンドを組み立て、すべてのコマンドにJSON Schemaがあります。execはゲストの終了ステータスをそのまま返します。
  - title: 安全なデフォルト
    details: 通信先の許可リスト、ゲストに渡らないシークレット、署名付きで期限のあるプレビュー、すべての接続の記録を備えています。
  - title: 見て操作できるデスクトップ
    details: defaultイメージにはxfceとChromiumが入っています。コンピューター操作エージェント用のスクリーンショットと入力に加え、ブラウザでライブビューを開けます。
  - title: セルフホスト
    details: ノートPCでもサーバーでも動きます。アカウントもクラウドもテレメトリーも不要です。ライセンスはApache-2.0です。
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
