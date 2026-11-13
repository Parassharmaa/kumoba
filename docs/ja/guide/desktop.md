# デスクトップのライブビューを開く

`default`イメージにはxfceデスクトップとChromiumが入っています。エージェントがスクリーンショットと入力で操作している様子を、ブラウザから見たり操作したりできます。

始める前に:

- [kumobaをインストールして`kumobad`を起動](/ja/getting-started)しておきます。
- ポート7789が空いていることを確認します。kumobadはこのポートでブラウザ用のリンクを提供します。

## ライブビューを開く

1. `default`イメージからサンドボックスを作って起動します:

   ```sh
   id=$(kumoba create --image default -q)
   kumoba start "$id"
   ```

2. デスクトップを起動し、ブラウザで開きます:

   ```sh
   kumoba desktop "$id" --user kumoba --open
   ```

   デスクトップが表示され、キーボードとマウスで操作できます。**Applications → Terminal Emulator**から、サンドボックス内のシェルを使えます。

リンクの有効期限は1時間です。期限が切れたら同じコマンドをもう一度実行してください。`--open`を付けない場合は、リンクが`url`として表示されます。

## リモートマシンのライブビューを開く

kumobadがサーバーで動いている場合は、手元のPCからプレビュー用のポートを転送します:

```sh
ssh -N -L 7789:127.0.0.1:7789 my-server &
```

サーバーで`kumoba desktop <id> --user kumoba`を実行し、表示された`url`を手元のブラウザで開きます。
