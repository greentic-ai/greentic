# 🤖 How to Create a Telegram Bot and Get Your `TELEGRAM_TOKEN`

To use the Telegram channel in Greentic, you'll need a **Telegram Bot Token**, known as `TELEGRAM_TOKEN`. You can generate it in under 2 minutes using **BotFather**, Telegram's official bot management interface.

---

## 🧰 Step-by-Step Instructions

### 1. Open BotFather

On your phone or desktop, click or go to:  
👉 [https://telegram.me/BotFather](https://telegram.me/BotFather)

You'll see this screen:

![BotFather Home](https://github.com/greentic-ai/greentic/blob/dev/assets/botfather.png)

---

### 2. Create a New Bot

Type the command:  
```
/newbot
```

BotFather will ask you to give your bot a **name** and a **username**. The name is what users see; the username must end in `bot`, e.g. `weather_helper_bot`.

You'll receive a response like:

![New Bot Created](https://github.com/greentic-ai/greentic/blob/dev/assets/newbot.png)

---

### 3. Copy Your Token

BotFather will respond with something like:
```
Done! Congratulations on your new bot. You will find it at telegram.me/your_bot.
Use this token to access the HTTP API:
123456789:ABCDEF_your_real_token_here
```

🔐 **Copy the token** — this is your `TELEGRAM_TOKEN`.

---

## ✅ Add Your Token to Greentic

Once you have your token, run:

```bash
greentic secrets add TELEGRAM_TOKEN 123456789:ABCDEF_your_real_token_here
```

Replace the example with your actual token.

---

## 🚀 You're Ready

Your Telegram bot is now connected to Greentic. You can use flows like:

```bash
greentic run
```

---

Need help? Join the community or contact support at [greentic.ai](https://greentic.ai).
