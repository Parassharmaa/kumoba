# はじめに

::: warning アルファ版
kumobaはアルファ版です。HTTP API、CLI、SDK、イメージ形式はリリース間で変わる可能性があり、プロトコルのバージョンが変わったときはゲストイメージを入れ替える必要があります。依存する用途ではバージョンを固定してください(`KUMOBA_VERSION=0.2.2`)。
:::

このページでは、kumobaをインストールし、最初のサンドボックスでコードを実行し、そのデスクトップをブラウザで開くところまでを説明します。

## 必要なもの

| ホスト | 必要なもの |
| --- | --- |
| macOS 14以降(Apple silicon) | Homebrew。下の手順でlibkrunを入れます |
| Linux(x86_64またはarm64) | `/dev/kvm`への書き込み権限と`zstd` |

空きディスク容量が約2GB必要です。

## 1. インストールする

1. macOSの場合は、先にlibkrunをインストールします:

   ```sh
   brew tap slp/krun && brew install slp/krun/libkrun slp/krun/libkrunfw zstd
   ```

   Linuxの場合は、`zstd`をインストールし、自分のユーザーを`kvm`グループに追加してから再ログインします:

   ```sh
   sudo apt install -y zstd
   sudo usermod -aG kvm "$USER"
   ```

2. kumobaをインストールし、`PATH`に追加します:

   ```sh
   curl -fsSL https://raw.githubusercontent.com/Parassharmaa/kumoba/main/scripts/install.sh | bash
   export PATH="$HOME/.kumoba/bin:$PATH"     # シェルのプロファイルにも追記してください
   ```

   `kumobad`デーモン、`kumoba` CLI、`kumoba-worker`、`default`イメージ(Python、Node、Chromium、xfceデスクトップ入りのDebian)が`~/.kumoba`に入ります。ダウンロードしたファイルはすべてリリースの`SHA256SUMS`で検証されます。

## 2. デーモンを起動する

ターミナルを1つ開き、次のコマンドを実行したままにします。設定は不要です:

```sh
kumobad
```

`kumobad`は`~/.kumoba/images`のイメージを読み込み、`~/.kumoba/token`にトークンを作り、`~/.kumoba/kumobad.sock`と`127.0.0.1:7788`でAPIを、`127.0.0.1:7789`でブラウザ用のリンクを提供します。止めるときはCtrl-Cを押すか、別のターミナルで`pkill -x kumobad`を実行します。

## 3. コードを実行する

別のターミナルで実行します:

```sh
kumoba run -- python3 -c 'import platform; print(platform.platform())'
```

`kumoba run`は新しいサンドボックスを作ってコマンドを1つ実行し、終わったら削除します。

サンドボックスを残して作業を続ける場合は、次のようにします:

```sh
id=$(kumoba create --image default -q)
kumoba start "$id" -q
kumoba exec "$id" -- pip install requests
echo 'print("hi")' | kumoba write-file "$id" --path /workspace/hi.py
kumoba exec "$id" --cwd /workspace -- python3 hi.py
kumoba shell "$id"                           # サンドボックス内のターミナル。exitで終了します
```

## 4. デスクトップをブラウザで開く

```sh
kumoba desktop "$id" --user kumoba --open
```

ブラウザでサンドボックスのxfceデスクトップが開き、キーボードとマウスで操作できます。**Applications → Terminal Emulator**から、サンドボックス内のシェルを使えます。

終わったらサンドボックスを削除します:

```sh
kumoba delete "$id"
```

## 5. コードから使う

```sh
npm install kumoba
```

```ts
import { Kumoba } from "kumoba";

const kumoba = new Kumoba();

const once = await kumoba.run("python3 --version");
console.log(once.stdout);

const sandbox = await kumoba.create({ user: "kumoba" });
await sandbox.files.write("/workspace/a.txt", "hello");
console.log((await sandbox.run("cat /workspace/a.txt")).stdout);
await sandbox.delete();
```

`new Kumoba()`は`~/.kumoba`のソケットとトークンを自動で見つけます。

## 次のステップ

- [コードを1回だけ実行する](/ja/guide/run-code)
- [デスクトップのライブビューを開く](/ja/guide/desktop)
- [トラブルシューティング](/ja/troubleshooting)
- コマンドの一覧は`kumoba`を引数なしで実行すると表示されます。詳しくは[CLIリファレンス(英語)](/cli)を参照してください。
