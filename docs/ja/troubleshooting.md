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

## `this sandbox's image has no shell support`と表示される

kumoba 0.3より前のイメージから作られたサンドボックスです。アップグレードしてから、新しいイメージでサンドボックスを作り直してください。

## `shell is interactive: use the kumoba CLI 0.3 or later`と表示される

お使いのCLIは`kumoba shell`に対応する前のバージョンです。インストーラーか`npm install -g kumoba@latest`で最新版をインストールしてください。

## `pip install`が`externally-managed-environment`で失敗する

kumoba 0.2.2より前のイメージです。アップグレードすると、`pip install`をそのまま使えるイメージになります。

## `exec`で起動したサーバーがすぐに止まる

`exec`は、コマンドが終わるとそのコマンドが起動したプロセスも止めます。動かし続けるプロセスは`setsid`で起動してください。

## バックグラウンドのプロセスの出力が返ってこない

コマンドが、出力を開いたままのプロセスをバックグラウンドに残しています(例: `sh -c 'make; ./server &'`)。`exec`はコマンドが終わってから 0.25 秒だけ出力を読み続け、それまでに届いた分を返して読むのをやめます。そのあとにそのプロセスが書いた分は返らず、`truncated`にも数えられません。出力は`./server > /tmp/server.log 2>&1 &`のようにファイルへ送り、そのファイルを読んでください。

## コマンドが終了ステータス141で途切れる

`kumoba exec`や`kumoba run`の出力を読む側が、途中でパイプを閉じました(例: `kumoba exec <id> -- cat big.log | head`)。パイプが閉じられたときの通常の終了ステータスです。

## macOS サンドボックスのライブビューが開かない

macOS サンドボックスで `kumoba desktop` を実行すると、`the live view did not start` と、かっこの中に理由が表示されます。

- **`kumoba-view-host is missing`**: macOS サンドボックスの画面を表示するエンジンが `kumobad` の隣にありません。インストールスクリプトはそこに置きますが、`cargo install` でビルドした `kumobad` にはありません。[リポジトリからビルドする](/install#build-the-live-view-engine-for-macos-sandboxes)か、`KUMOBAD_VIEW` に場所を設定してください。
- **`the live view needs kumobad to run inside a logged-in desktop session`**: `kumobad` を SSH から起動したか、起動時のデーモンとして動かしています。ログイン中のデスクトップセッションのターミナルから起動してください。
- **`too many live views are open on this sandbox`**: 1 つの macOS サンドボックスを同時に見られるページは 8 つまでです。いくつか閉じてください。

貼り付け、コピー、US 配列にない文字の入力には、サンドボックスのデスクトップに誰かがログインしている必要があります。ログイン前は何も起きず、`~/.kumoba/state/runtime/sandboxes/<id>/worker.log` に `nobody is signed in to the sandbox's desktop` と記録されます。

ページが開いて macOS のログイン画面が表示される場合、それがサンドボックスの画面です。まだ誰もログインしていません。`~/.kumoba/images/macos/password` のパスワードで `kumoba` としてログインしてください。

## macOS サンドボックスの起動で `running sandbox capacity exhausted` になる

Apple の制限により、1 台の Mac で同時に動かせる macOS の VM は 2 台までです。UTM や Tart など他のアプリの VM も数に入ります。macOS サンドボックスを 1 つ止めるか、他のアプリの VM を終了してから、もう一度起動してください。詳しくは[サンドボックスで macOS を動かす](/ja/guide/macos#制限)を参照してください。

## kumoba をアップグレードしたら macOS サンドボックスが起動しない

`this macOS sandbox's guest agent is from another kumoba release` は、サンドボックスのディスクにクローン時のエージェントが残っていることを示します。`kumobad macos-image` を実行してください。完成したイメージに対しては、現在のエージェントを入れ直し、それ以外はそのまま残します。それより前に作ったサンドボックスは自分のディスクに古いエージェントを持ち続けるので、必要なものを取り出してから作り直してください。[サンドボックスで macOS を動かす](/ja/guide/macos#イメージを更新する)を参照してください。

## `kumobad macos-image` が途中で止まる

もう一度実行してください。終わった段階は `~/.kumoba/images/macos/build.json` に記録されているので続きから再開し、中断したダウンロードも途中から再開します。`guest provisioning needs macOS 27 or later on the host` と表示された場合は、先に Mac 本体を macOS 27 以降に更新する必要があります。

## それでも解決しない場合

kumobadのログ、`kumoba version`の出力、OSとCPUを添えて[Issue](https://github.com/Parassharmaa/kumoba/issues)を作成してください。セキュリティの問題は公開せずに報告してください。
