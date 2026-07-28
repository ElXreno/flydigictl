# Changelog

## [0.1.2](https://github.com/ElXreno/flydigictl/compare/v0.1.1...v0.1.2) (2026-07-28)


### Bug Fixes

* give the interface its own archive on both architectures ([d0406ed](https://github.com/ElXreno/flydigictl/commit/d0406ed8ff25e22a967eb5bce35bcf644dedd486))

## [0.1.1](https://github.com/ElXreno/flydigictl/compare/v0.1.0...v0.1.1) (2026-07-28)


### Bug Fixes

* ship the daemon and the interface in the packages ([fba3a88](https://github.com/ElXreno/flydigictl/commit/fba3a88ddc40161bb7b6e3dbad42702f37a30f24))

## 0.1.0 (2026-07-28)


### Features

* add a fan curve daemon with a config watcher and control socket ([c9c6bdb](https://github.com/ElXreno/flydigictl/commit/c9c6bdb723f62cbd018a9599eaaabf1aef255115))
* allow stopping the fan, lower the floor to a measured 500 rpm ([0461182](https://github.com/ElXreno/flydigictl/commit/0461182b43acc3bd8875c85dafa7c1a3d9e38abf))
* control Flydigi BS series coolers over hidraw ([efa6da4](https://github.com/ElXreno/flydigictl/commit/efa6da4cf5a3266c653f34e016c455cd2cf36130))
* edit curves and lighting from the interface ([16806a8](https://github.com/ElXreno/flydigictl/commit/16806a84ce4256d484236582529bf61e8c63371d))
* expose built-in light effects and their realtime gate ([63361d5](https://github.com/ElXreno/flydigictl/commit/63361d52d467a48553b3e4f3cbda2695c1af6f8e))
* expose gears, lighting and standby to clients ([082383d](https://github.com/ElXreno/flydigictl/commit/082383d26807fee243373b45a778f458b4502643))
* expose the cooler's standby mode and give warnings stable codes ([cc031d1](https://github.com/ElXreno/flydigictl/commit/cc031d1ec6885f18e8b6309a9e634b6222819bf0))
* follow a GPU that is awake and leave a sleeping one alone ([0dc07ec](https://github.com/ElXreno/flydigictl/commit/0dc07ec214b6c6d486a1af0f161b617202d89b42))
* honour the cooler supply level ([23ad787](https://github.com/ElXreno/flydigictl/commit/23ad7872727d85d70c1c8328602e67466388ffea))
* keep watching across stream gaps and reconnects ([cd6fe88](https://github.com/ElXreno/flydigictl/commit/cd6fe88f9bd11ebacec4b9b43bccc89d4ffe963f))
* one curve per subsystem with smoothed inputs ([6132439](https://github.com/ElXreno/flydigictl/commit/6132439eb1acd01b6d8ef836bb45fac8367fde86))
* package the desktop interface ([9621bcb](https://github.com/ElXreno/flydigictl/commit/9621bcbfe52c5c199b860ae58e05fc03510c8003))
* paint the strip a static colour with brightness control ([a99231e](https://github.com/ElXreno/flydigictl/commit/a99231ed1c06c83efb38031634b258f3af865b3c))
* put the lights out while the screens are ([049d0f9](https://github.com/ElXreno/flydigictl/commit/049d0f9d8b5a1f86068c32bafc9eaea2199eca99))
* read a palette and ship a home-manager module ([0db2e20](https://github.com/ElXreno/flydigictl/commit/0db2e209c02dffd06129ec5170b8078ef50f7a98))
* read and write the stored gear speeds ([b6fe56c](https://github.com/ElXreno/flydigictl/commit/b6fe56c6dee263ec9e99176419fa34dfef7d3820))
* read several sensors and keep retrying the missing ones ([4d5f22e](https://github.com/ElXreno/flydigictl/commit/4d5f22e423fe71e17c23cf3d8a66b2dfe41f6dc2))
* read the GPU off its own registers instead of waking it ([85ec5dc](https://github.com/ElXreno/flydigictl/commit/85ec5dcf8e7bc9d3eab4085ddfaf5b0ed0d2c4f9))
* read the strip power flag and stop sending reports that do nothing ([d3336fb](https://github.com/ElXreno/flydigictl/commit/d3336fbb8ba57be83a0ab8977712f261bb08e39f))
* step back through configuration changes ([394e772](https://github.com/ElXreno/flydigictl/commit/394e772d7e69998ca4e616417bdab7f16948ba14))
* stop the fan when every curve says the machine is cold ([742f7d2](https://github.com/ElXreno/flydigictl/commit/742f7d2908f88790cd7c79e8d2f19331feb46c6b))
* stream light buffers with the block upload commands ([b9db1be](https://github.com/ElXreno/flydigictl/commit/b9db1be3ca61997258ed124f18e669200e707682))
* stream status to clients and add a curve editor ([a613d97](https://github.com/ElXreno/flydigictl/commit/a613d976e9454b63ebd55849111c511fab6e8910))
* take the socket from systemd and drop root ([ddd9953](https://github.com/ElXreno/flydigictl/commit/ddd99536e0b955c3fc887f1dc63c5a315aa7711e))
* upload preset palettes so effects survive gear mode ([0b71578](https://github.com/ElXreno/flydigictl/commit/0b7157808b5471d107276450bb627efb6268a635))
* verify every command against the device acknowledgement ([8eae7cf](https://github.com/ElXreno/flydigictl/commit/8eae7cf67442c1d1218390937b02713b20d3bf40))


### Bug Fixes

* address sensors by slot rather than by probe order ([1ecd8c9](https://github.com/ElXreno/flydigictl/commit/1ecd8c9a6ed349ff4d6ed26e3ad4c516c7599163))
* cap fan speed at the rated 4000 rpm ([887454c](https://github.com/ElXreno/flydigictl/commit/887454c9710504b416a0385490a74e48786a1c7c))
* give the curve buttons a line of their own ([4c39949](https://github.com/ElXreno/flydigictl/commit/4c399495e2ab39a136a158c1e6be4648a15c122d))
* hold the cooler to the speed it was given ([55fdd02](https://github.com/ElXreno/flydigictl/commit/55fdd0251a43b96baa00ea249e51eb6f2fb8564c))
* keep standing warnings on screen while they hold ([f36c0b3](https://github.com/ElXreno/flydigictl/commit/f36c0b38c2098d3a8fbc4037b6695f114be2bf29))
* keep the cooler's flash out of the reconnect path ([6f6cb57](https://github.com/ElXreno/flydigictl/commit/6f6cb5710a8c7c1c9892bf40b2b95e0a6998cbe5))
* keep the window responsive while the daemon works ([aaffcce](https://github.com/ElXreno/flydigictl/commit/aaffccee90e847c407905cb3a280b57d6d38eb9d))
* name a card curve after the card, and lead nothing when the fan is off ([7b584a3](https://github.com/ElXreno/flydigictl/commit/7b584a3d5869e9e47829d6eeba3979a63d725ff8))
* parse the mode byte as a bitfield, not an enum ([3f06814](https://github.com/ElXreno/flydigictl/commit/3f068142c949921fddf7ecfa855de0b239b591e9))
* require the socket unit instead of binding a socket we cannot create ([a7f131f](https://github.com/ElXreno/flydigictl/commit/a7f131f39362460e6759660c399dd5d1c07f4847))
* stop hiding network sensors from the daemon ([827e4d8](https://github.com/ElXreno/flydigictl/commit/827e4d8c49b688ee914caa73a287f1cb65264a1e))
* stop rewriting a speed the cooler already holds ([6ce026d](https://github.com/ElXreno/flydigictl/commit/6ce026d54ba38ce505403e4c3a0305170507bb05))
* tell clients when the config changed under them ([a178478](https://github.com/ElXreno/flydigictl/commit/a178478038f0007502bd1eba5d32406ea3bf4582))
* tell identical sensors apart and stop clients clobbering runtime state ([3ed46b6](https://github.com/ElXreno/flydigictl/commit/3ed46b6765ca9d2e534f21f0dde3d3f3e8256078))
* write the manual speed when the slider is let go ([252b304](https://github.com/ElXreno/flydigictl/commit/252b3047324f4a60970eb9c31ecdf4d99ea78a8b))
