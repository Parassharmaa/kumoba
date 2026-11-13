# コードを1回だけ実行する

結果だけが欲しいときは`kumoba run`を使います。サンドボックスを作ってコマンドを1つ実行し、出力を表示してからサンドボックスを削除します。何も残りません。

始める前に、[kumobaをインストールして`kumobad`を起動](/ja/getting-started)しておきます。

1. コマンドを実行します:

   ```sh
   kumoba run -- python3 -c 'print("hello from a fresh VM")'
   ```

   `--`より後ろがプログラムと引数です。`python3`のようなコマンド名は、ゲストの`PATH`から探されます。

2. 終了ステータスを確認します。`kumoba run`はコマンド自身の終了ステータスで終わるので、スクリプトやエージェントで分岐に使えます:

   ```sh
   kumoba run -- sh -c 'exit 7'; echo $?      # 7
   ```

## ネットワークなしで実行する

デフォルトではサンドボックスからインターネットに接続できます。信頼できないコードをネットワークなしで実行するには:

```sh
kumoba run --network.mode blocked -- python3 untrusted.py
```

特定のホストだけを許可するには、ホストを並べます。`*.example.com`はサブドメインにも一致します:

```sh
kumoba run --network.mode allowlist --network.hosts example.com -- curl -sS https://example.com
```

## 時間とリソースを制限する

```sh
kumoba run --timeout-ms 30000 --cpu 1 --memory-mib 1024 -- python3 job.py
```

タイムアウトを過ぎたコマンドは停止され、`kumoba run`は終了ステータス124で終わります。

## コードから使う

```ts
import { Kumoba } from "kumoba";

const result = await new Kumoba().run("python3 --version");
console.log(result.exitCode, result.stdout);
```
