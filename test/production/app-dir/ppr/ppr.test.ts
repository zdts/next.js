import { createNextDescribe } from 'e2e-utils'

createNextDescribe(
  'ppr',
  {
    files: __dirname,
  },
  ({ next }) => {
    // When we're partially pre-rendering, we should get the static parts
    // immediately, and the dynamic parts after the page loads. So we should
    // see the static part in the output, but the dynamic part should be
    // missing.
    it('should serve the static part', async () => {
      const $ = await next.render$('/suspense/node')
      expect($('#page').length).toBe(1)
    })

    it('should not have the dynamic part', async () => {
      const $ = await next.render$('/suspense/node')
      expect($('#dynamic').length).toBe(0)
    })

    describe.each([
      { name: 'node', pathname: '/suspense/node' },
      { name: 'edge', pathname: '/suspense/edge' },
    ])('for $name', ({ pathname }) => {
      // When the browser loads the page, we expect that the dynamic part will
      // be rendered.
      it('should eventually render the dynamic part', async () => {
        const browser = await next.browser(pathname)

        try {
          // Wait for the page part to load.
          await browser.waitForElementByCss('#page')

          // Wait for the dynamic part to load.
          await browser.waitForElementByCss('#dynamic')

          // Ensure we've got the right dynamic part.
          let dynamic = await browser.elementByCss('#dynamic').text()

          expect(dynamic).toBe('Signed Out')

          // Re-visit the page with the cookie.
          await browser.addCookie({ name: 'session', value: '1' })
          await browser.refresh()

          // Wait for the page part to load.
          await browser.waitForElementByCss('#page')

          // Wait for the dynamic part to load.
          await browser.waitForElementByCss('#dynamic')

          // Ensure we've got the right dynamic part.
          dynamic = await browser.elementByCss('#dynamic').text()

          expect(dynamic).toBe('Signed In')
        } finally {
          await browser.deleteCookies()
          await browser.close()
        }
      })
    })
  }
)
