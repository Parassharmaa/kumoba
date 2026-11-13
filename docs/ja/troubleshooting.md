# トラブルシューティング

## インストーラーがlibkrunが見つからないと表示する(macOS)

Homebrewでインストールしてから、インストーラーをもう一度実行します:

```sh
brew tap slp/krun && brew install slp/krun/libkrun slp/krun/libkrunfw zstd
```

## `/dev/kvm`に書き込めない(Linux)

kumobadにはKVMが必要です。自分のユーザーを`kvm`グループに追加して、再ログインします:

```sh
sudo usermod -aG kvm "$USER"
```

クラウドのVMでは、インスタンスタイプがネストされた仮想化に対応しているか確認してください。

## インストーラーがイメージを展開できない

ディスクの空き容量が不足しています。`default`イメージの展開には約2GB必要です。空き容量を確保してから、インストーラーをもう一度実行してください。途中まで書き込まれたファイルはインストーラーが削除します。

## `kumoba: Cannot reach kumobad`と表示される

kumobadが起動していないか、CLIが別の場所を見ています。`kumobad`で起動してください。独自の設定でkumobadを動かしている場合は、`KUMOBA_URL`と`KUMOBA_TOKEN`でCLIの接続先を指定します。この場合、CLIは終了ステータス3で終わります。

## `No kumobad token`と表示される

kumobadは初回の起動時に`~/.kumoba/token`を作ります。一度起動するか、`KUMOBA_TOKEN`を設定してください。

## デスクトップのリンクが開かない、またはkumobadが`preview_listen_skipped`を記録する

ポート7789を別のプログラムが使っているため、ブラウザ用のリンクを提供できません。ポートを空けるか、別のポートを指定します: `KUMOBAD_PREVIEW_LISTEN=127.0.0.1:7790 kumobad`。ポート7788が使われているときは`listen_skipped`が記録されますが、CLIとSDKはソケット経由で動作し続けます。

## `stop the sandbox before changing its network`と表示される

ネットワークの設定は起動時に適用されます。`kumoba stop <id>`、`kumoba update`、`kumoba start <id>`の順に実行してください。

## `pip install`が`externally-managed-environment`で失敗する

kumoba 0.2.2より前のイメージです。アップグレードすると、`pip install`をそのまま使えるイメージになります。

## `exec`で起動したサーバーがすぐに止まる

`exec`は、コマンドが終わるとそのコマンドが起動したプロセスも止めます。動かし続けるプロセスは`setsid`で起動してください。

## それでも解決しない場合

kumobadのログ、`kumoba version`の出力、OSとCPUを添えて[Issue](https://github.com/Parassharmaa/kumoba/issues)を作成してください。セキュリティの問題は公開せずに報告してください。
