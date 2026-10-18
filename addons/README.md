# Wave Defense addon

This branch includes the optional WaveDefense addon for the LyraCore cooperative defense mode.
Benilla's engine is unchanged. The addon uses the ordinary 1.12 addon API and server messages.

From the checkout, install it into your local client state:

```bash
mkdir -p benilla-config/AddOns
cp -R addons/WaveDefense benilla-config/AddOns/
```

For a player build, copy `WaveDefense` into `benilla-config/AddOns/` beside the executable instead.
Enable it in the addon list and use `/wd` after entering the defense realm.
The server address and account are configured through the ordinary login screen.

The canonical addon source is [LyraCore Packages at df14a41c](https://github.com/LyraCoreProject/packages/tree/df14a41c1378a23af2fab85e0a493259c6ebb3f2/wave-defense).
Keep these four files identical to that source when updating this branch. The Package README
contains the server setup, game rules and prototype limits. No game assets are included.
