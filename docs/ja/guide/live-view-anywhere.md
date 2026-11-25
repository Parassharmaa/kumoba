# どのコンテナでもライブビューを使う

kumobaのライブビューのエンジン`kumoba-view`は単体でも動きます。Dockerコンテナ、VM、他社のサンドボックスなど、Xのディスプレイがある任意のLinuxデスクトップで、VNCの代わりに使えます。依存のない静的なバイナリ1つで動きます。ビューアのページもエンジン自身が配信するので、ブラウザには何も入れる必要がなく、websockifyやnoVNCを横で動かす必要もありません。

## 入手する

0.6.3以降の各リリースに、x86_64とarm64のLinux用エンジンがあります。チェックサムは`SHA256SUMS`にあります:

```sh
arch=$(uname -m | sed 's/aarch64/arm64/')
curl -fsSL -o /usr/local/bin/kumoba-view \
  "https://github.com/Parassharmaa/kumoba/releases/latest/download/kumoba-view-linux-$arch"
chmod +x /usr/local/bin/kumoba-view
```

## 起動する

Xサーバーと同じマシンで、表示するディスプレイを`DISPLAY`に指定して起動します:

```sh
DISPLAY=:0 kumoba-view --listen 0.0.0.0:6090
```

開くアドレスが表示されます。末尾は`?token=`と、エンジンが作ったトークンです。ターミナルで動かしていないとき(`docker run -d`など)は、トークンは表示されず、`~/.kumoba-view-token-6090`(末尾はポート番号)に書き込まれます。`docker run -t`では表示され、`docker logs`に残ります:

```text
kumoba-view listening on 0.0.0.0:6090; open http://<this host>:6090/?token=4f0c…
```

そのアドレスを、マシンに届くホスト名とポートでブラウザに開きます。キーボード、マウス、スクロールは[ライブビューのガイド](/ja/guide/desktop)のとおりに使えます。貼り付けとコピーは、一般的なクリップボードのショートカットを使うアプリで使えます。下のDockerの例にある素の`xterm`では使えません。

- **トークン。** トークンを知っている人は、画面を見て、キーボードとマウスを操作できます。自分で決めるときは`KUMOBA_VIEW_TOKEN`を設定するか、`--token-file`でトークンを書いたファイルを渡します。トークンに使えるのは英数字と`.`、`_`、`~`、`-`です。マシンの他のユーザーから見えるコマンドラインでは受け取らず、自分で決めたトークンはログに表示しません。自分で決めたトークンは、ループバックでも必要になります。組み立てたリンクに入れたい場合などに使います。`127.0.0.1`で待ち受けるときは、そのマシンからしか接続できないのでトークンは要りません。
- **見るだけのリンク。** 操作はさせずに画面だけ見せたいときは、2つ目のトークンを`KUMOBA_VIEW_WATCH_TOKEN`で設定するか、`--watch-token-file`でそれを書いたファイルを渡します。規則はトークンと同じで、トークンとは別の値にします。`?token=`に見るだけのトークンを付けて開いたストリームは画面を表示し、そのキーボード、マウス、クリップボード、入力したテキストはエンジン自体が無視します。ページを書き換えたり手で接続したりしても操作はできません。ビューアーページには「Watching only」と表示されます。トークンでは引き続き操作できます。見るだけのトークンを設定すると、ループバックでもトークンが必要になります。トークンを決めていないときは、エンジンが作って上と同じように表示するか保存します。`unix:`ソケットはトークンを使わないので、そこでは見るだけのトークンを受け付けず起動しません。
- **暗号化。** エンジンはHTTPで配信します。信頼できないネットワーク越しに使うときは、WebSocketのアップグレードを転送するHTTPSのリバースプロキシの後ろに置いてください。ブラウザは、`https`のページか`localhost`でしか貼り付けとコピーを許可しません。
- **必要なもの。** 同じマシンで動くXサーバーと、そのMIT-SHM、DAMAGE、XFIXES、XTEST拡張です。Xvfb、Xorg、Xvncはどれも備えています。そのディスプレイの画面全体を表示します。
- **オプション。** `--fps`は1秒あたりの最大フレーム数(既定は60)、`--max-mbps`は最大帯域(既定は30 Mbit/s)です。`kumoba-view --help`で一覧を表示できます。

## Dockerで試す

このイメージは、XvfbとOpenboxのウィンドウマネージャーとターミナルで小さなデスクトップを動かし、ライブビューで表示します:

```dockerfile
FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends xvfb openbox xterm curl ca-certificates \
 && rm -rf /var/lib/apt/lists/*
RUN arch=$(uname -m | sed 's/aarch64/arm64/') \
 && curl -fsSL -o /usr/local/bin/kumoba-view \
    "https://github.com/Parassharmaa/kumoba/releases/latest/download/kumoba-view-linux-$arch" \
 && chmod +x /usr/local/bin/kumoba-view
ENV DISPLAY=:0
CMD Xvfb :0 -screen 0 1280x800x24 & while [ ! -e /tmp/.X11-unix/X0 ]; do sleep 0.1; done; openbox & xterm & exec kumoba-view --listen 0.0.0.0:6090
```

```sh
docker build -t desktop .
docker run --rm --init -p 6090:6090 -e KUMOBA_VIEW_TOKEN=change-me desktop
```

そのあと`http://localhost:6090/?token=change-me`を開きます。`--init`を付けると、Ctrl+Cや`docker stop`ですぐに終了します。Openboxとxtermの代わりに、`xfce4`やブラウザなど、使いたいデスクトップとアプリを入れてください。

## 自分のページに表示する

ビューアーのページは、ストリームを見る方法の1つです。自分のWebアプリの中にデスクトップを表示するときは、`kumoba` npmパッケージのブラウザクライアントを使います。ビューアーのページと同じクライアントで、デコーダーを内蔵しているので、横に置くファイルはありません:

```sh
npm install kumoba
```

```ts
import { connectViewer } from "kumoba/view";

const canvas = document.querySelector("canvas")!;
const viewer = connectViewer(canvas, "wss://desktop.example/stream?token=change-me", {
  onConnect: () => console.log("the first frame is on screen"),
  onClose: ({ connected, error }) => console.log("the stream ended", connected, error),
  onClipboard: (text) => navigator.clipboard.writeText(text),
});
addEventListener("keydown", (event) => viewer.key(event));
addEventListener("keyup", (event) => viewer.key(event));
```

- **バージョン。** エンジンと同じリリースの`kumoba`パッケージを使ってください。ストリームの形式はリリースの間で変わることがあります。
- **サイズ。** キャンバスはデスクトップのピクセルサイズになります。大きさはCSSで決めます。`object-fit: contain`なら縦横比を保ち、ポインターの位置もずれません。デスクトップの周りの空いた帯でのクリックはページ側のもので、送られません。
- **入力。** マウスのボタンとホイールはキャンバスから、キーは`viewer.key`に渡したものから送ります。`typeText`は入力メソッドが確定したテキストなどを入力し、`paste`はテキストをデスクトップのクリップボードに置いて貼り付けのショートカットを押します。`releaseHeldInput`は、ページからフォーカスが外れたときなどに、押したままのキーとボタンを離します。操作中はキャンバスに`touch-action: none`を設定するので、タッチのドラッグはページのスクロールではなくデスクトップ上のドラッグになります。見るだけに戻すか閉じると、元の値に戻ります。
- **見るだけ。** `viewOnly: true`ではストリームを表示し、入力は送りません。あとから`setViewOnly`で切り替えられます。これはブラウザーの中だけの制限なので、ページを書き換えられる人は入力を送れます。確実に操作させたくない相手には[見るだけのリンク](#起動する)を渡します。見るだけのトークンで開いたストリームでは、始まると`watchOnly()`が`true`を返し、クライアントは見るだけのままになり、`setViewOnly(false)`は効きません。
- **Content Security Policy。** デコーダーはWebAssemblyなので、ポリシーのあるページでは`script-src`に`'wasm-unsafe-eval'`が必要です。
