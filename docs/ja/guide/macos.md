# サンドボックスで macOS を動かす

Apple シリコンの Mac では、サンドボックスで Linux の代わりに macOS を動かせます。Apple のコンパイラでのビルドとテスト、macOS 用のビルドスクリプトの実行、コマンドラインツールの macOS での確認など、本物の Mac が必要な作業に使います。CLI、SDK、ネットワークルール、シークレットは Linux と同じように使えます。

macOS サンドボックスは kumoba 0.4 で追加された機能で、kumoba 全体と同じくアルファ版です。

## 始める前に

- macOS 27 以降の Apple シリコン Mac に、kumoba 0.4 以降を[インストール](/ja/getting-started)してあること。
- イメージの作成中に約 50 GB の空き容量。完成したイメージは約 28 GB です。

## macOS イメージを作る

1. 次のコマンドで作成します。

   ```sh
   kumobad macos-image
   ```

   お使いの Mac 向けの macOS 復元イメージ（約 26 GB）を Apple からダウンロードし、macOS をインストールして `kumoba` ユーザー、Apple の Command Line Tools、ゲストエージェントを用意します。ダウンロードを除いて約 30 分かかり、インストールが終わると復元イメージは削除されます。途中で止まった場合は、もう一度実行すると続きから再開します。完成したイメージがある場合は何もしないので、作り直すときは `~/.kumoba/images/macos` を削除してください。

2. `kumobad` を再起動すると新しいイメージが読み込まれ、`kumoba images` に `macos` が表示されます。

## macOS サンドボックスを使う

```sh
id=$(kumoba create --image macos -q)
kumoba start "$id" -q                          # macOS の起動に 20〜30 秒ほどかかります
kumoba exec "$id" -- sw_vers
kumoba exec "$id" --user kumoba --cwd /workspace -- sh -c 'echo hi > note.txt; ls'
kumoba shell "$id" --user kumoba               # 対話型の zsh
kumoba delete "$id"
```

各サンドボックスは独自のマシン ID を持つ、イメージのコピーオンライトのクローンなので、作成は 1 秒もかからず、変更するまでディスクもほとんど使いません。macOS サンドボックスは常にイメージの CPU 4 個とメモリ 4 GB で動き、指定した `cpu` と `memoryMib` は無視されます。

サンドボックスの中では次のようになります。

- `--user kumoba` を付けない限り、コマンドは root で動きます。`kumoba` ユーザーはパスワードなしで `sudo` を使えます。
- `/workspace` があり、所有者は `kumoba` です。
- ゲストの時計は起動のたびに Mac の時刻に合わせられ、誤差は 1 秒程度です。
- Apple の Command Line Tools が入っているので、`git`、`clang`、`make`、`python3` が使えます。Xcode 本体は含まれていません。

## ネットワークとシークレット

macOS サンドボックスにはネットワークカードがありません。通信はすべて kumobad のプロキシを通るので、[ネットワークルール](/guide/network)、[シークレット](/guide/secrets)、アクセスログは Linux と同じように働きます。

`curl`、`pip`、`npm` のように `HTTPS_PROXY` に従うコマンドラインツールはネットワークに届きます。Safari や Apple のネットワークフレームワークを使う多くのアプリはこの設定に従わないため、ネットワークに接続できません。

## 制限

- **同時に 2 台まで。** Apple の制限により、1 台の Mac で同時に動かせる macOS の VM は 2 台までです。UTM や Tart など他のアプリの VM も数に入ります。3 台目を起動すると `running sandbox capacity exhausted` で失敗します。Linux サンドボックスは数に入りません。
- **Apple のライセンス。** macOS を VM で動かせるのは Apple のハードウェア上だけで、用途はソフトウェア開発、テスト、macOS Server、個人の非営利利用に限られます。サービスを作る前に macOS のライセンスを確認してください。
- **未対応:** デスクトップのライブビュー、ホストのディレクトリのマウント、スナップショット。
- **ローカルのみ。** Linux ホストやリリース用 CI では macOS ゲストを動かせません。リリース前には Mac 上で `scripts/macos-smoke.sh` を実行して確認します。

## ファイルの場所

| パス | 内容 |
| --- | --- |
| `~/.kumoba/images/macos/` | イメージ本体（ディスク、補助ストレージ、`machine.json`） |
| `~/.kumoba/images/macos/password` | `kumoba` ユーザーのパスワード（本人だけが読めます） |
| `~/.kumoba/cache/` | ダウンロード中とインストール中の復元イメージ |
