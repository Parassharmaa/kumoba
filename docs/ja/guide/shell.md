# サンドボックスでシェルを開く

`kumoba shell`を使うと、`docker exec -it`のようにサンドボックス内で対話型のターミナルを使えます。矢印キー、タブ補完、Ctrl-C、全画面のターミナルプログラムが動作し、ターミナルの大きさはウィンドウに合わせて変わります。

始める前に、[kumobaをインストールして`kumobad`を起動](/ja/getting-started)し、起動済みのサンドボックスのIDを`$id`に入れておきます。

1. シェルを開きます:

   ```sh
   kumoba shell "$id"
   ```

   ゲストユーザーのログインシェルが`/workspace`で起動します。コマンドには`kumoba exec`と同じネットワーク設定とシークレットが適用されます。

2. `exit`またはCtrl-Dで終了します。`kumoba shell`はシェル自身の終了ステータスで終わります。

## ユーザー、ディレクトリ、プログラムを指定する

```sh
kumoba shell "$id" --user kumoba                 # イメージのkumobaユーザーでログインする
kumoba shell "$id" --cwd /tmp                    # 別のディレクトリで始める
kumoba shell "$id" --env DEBUG=1                 # 環境変数を追加する
kumoba shell "$id" -- python3                    # シェルの代わりにプログラムを実行する
```

## スクリプトから使う

標準入力がターミナルでない場合、`kumoba shell`は入力を1行ずつ送り、入力が終わるとセッションを終了します:

```sh
printf 'cd /workspace && ls\nexit\n' | kumoba shell "$id"
```

スクリプトでは、通常は`kumoba exec`のほうが適しています。標準出力と標準エラー出力が分かれ、入力がエコーされません。

## 必要なもの

サンドボックスのイメージはkumoba 0.3以降のものである必要があります。古いイメージでは`this sandbox's image has no shell support`と表示されます。アップグレードすると新しいイメージに入れ替わります。CLIも0.3以降が必要です。
