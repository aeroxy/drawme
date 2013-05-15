var socket = io.connect('http://localhost/');
var winWidth = $(window).width();
function animation(){
	$('.title h1').animate({
		'font-size':'2em',
	},1500);
	$('.title p').animate({
		'opacity':1,
	},1500);
}
$(document).ready(function(){
	animation();
	$('.canvas').css({
		'margin-left': (winWidth - 960)/2,
	});
	$('.options').css({
		'margin-left': (winWidth -202)/2,
	});
	$('#canvas').drawMe();
	$('#clear').click(function(){
        mixpanel.track('Clear Canvas');
		$('#canvas').data("drawMe").clear();
		socket.emit('clear');
		$('#notification').text('You just clear the canvas!').fadeIn(500).delay(1500).fadeOut(500);
	});
	socket.on('clear',function(){
		$('#canvas').data("drawMe").clear();
		$('#notification').text('Someone just clear the canvas!').fadeIn(500).delay(1500).fadeOut(500);
	});
	$('#save').click(function(){
        mixpanel.track('Save Canvas');
		$('#canvas').data("drawMe").save();
	});
});
$(window).resize(function(){
	var winWidth = $(window).width();
	$('.canvas').css({
		'margin-left': (winWidth - 960)/2,
	});
	$('.options').css({
		'margin-left': (winWidth -202)/2,
	});
});
function drawMeBrush(){
	drawMeBrush.prototype._init = function(context, brushSize, brushColor){
		this.context = context;
		this.context.globalCompositeOperation = 'source-over';
		this.brushSize = brushSize;
		this.brushColor = brushColor;
		this.drawn = false;
		this.active = false;
	};	
	drawMeBrush.prototype.strokeBegin = function(x, y){
		this.active = true;
		this.context.beginPath();
		this.context.lineWidth = this.brushSize;
	};	
	drawMeBrush.prototype.strokeMove = function(x, y){this.drawn = this.active;};	
	drawMeBrush.prototype.strokeEnd = function(){
		this.active = false;
		if(this.drawn){
			this.drawn = false;
			return true;
		}
		return false;
	};
}
BasicBrush.prototype = new drawMeBrush;
function BasicBrush(){
	BasicBrush.prototype.strokeBegin = function(x, y){
		drawMeBrush.prototype.strokeBegin.call(this, x, y);
		this.prevX = x; 
		this.prevY = y;
	};
	BasicBrush.prototype.strokeMove = function(x, y){
		drawMeBrush.prototype.strokeMove.call(this, x, y);
					
		this.context.moveTo(this.prevX, this.prevY);
		this.context.lineTo(x, y);
		
		this.context.strokeStyle = this.brushColor;
		this.context.stroke();
		
		this.prevX = x;
		this.prevY = y;
	};
}
function BasicCanvasSave(imageData){window.open(imageData,'drawMe Image');}
(function($){
	var settings = {
		width:				960,
		height: 			720,
		backgroundColor:	"#ffffff",
		saveMimeType: 		"image/png",
		saveFunction: 		BasicCanvasSave,
		brush:				BasicBrush,
		brushSize:			2,
		brushColor:			"rgb(0,0,0)"
	};
    var drawMe = function(elm, options){
        var $elm = $(elm);
        var self = this;
        var noparent = $elm.is('canvas');
        var canvas = noparent ? $elm[0] : document.createElement('canvas');
        var context = canvas.getContext('2d');
        var width = $elm.innerWidth();
        var height = $elm.innerHeight();
        $.extend(settings, options);
        if(noparent){
            width = $elm.parent().width();
            height = $elm.parent().height();
        }
        else $elm.append(canvas);
        if(width < 2)width = settings.width;
        if(height < 2)height = settings.height;
        self.blank = true;
        self.canvas = canvas;
        self.canvas.width = width;
        self.canvas.height = height;
        self.clear();
        if(settings.backgroundImage){
            addImage(context);
            self.blank = false;
        }
        self.brush = new settings.brush();
        self.brush._init(context, settings.brushSize, settings.brushColor);
        if(self.brush.strokeBegin && self.brush.strokeMove && self.brush.strokeEnd){
            canvas.addEventListener('touchstart', function(e){
                var o = $elm.offset();
                e.preventDefault();
                if(e.touches.length > 0)self.brush.strokeBegin(e.touches[0].pageX-o.left, e.touches[0].pageY-o.top);

            }, false);
            canvas.addEventListener('touchmove', function(e){
                var o = $elm.offset();
                e.preventDefault();
                if(e.touches.length > 0 && self.brush.active)self.brush.strokeMove(e.touches[0].pageX-o.left, e.touches[0].pageY-o.top);

            }, false);
            canvas.addEventListener('touchend', function(e){
                e.preventDefault();
                if(e.touches.length == 0)self.blank = !self.brush.strokeEnd() && self.blank;
            }, false);
            var mousedown = false;
            $(canvas).bind({
                mousedown: function(e){
                    mixpanel.track('Click Canvas');
                    mousedown = true;
                    var o = $elm.offset();
                    self.brush.strokeBegin(e.pageX-o.left, e.pageY-o.top);
                    socket.emit('start', e.pageX, e.pageY);
                    if($('.title p').css('opacity')!=0){
                        $('.title p').fadeOut(1000);
                        $('h1').animate({'font-size':'3em'},1000);
                    }
                },
                mousemove: function(e){
                    if(mousedown){
                    var o = $elm.offset();
                    if(self.brush.active)self.brush.strokeMove(e.pageX-o.left, e.pageY-o.top);
                    socket.emit('move', e.pageX, e.pageY);
                }
                },
                mouseup: function(e){
                    mousedown = false;
                    self.blank = !self.brush.strokeEnd() && self.blank;
                    socket.emit('stop');
                },
                mouseout: function(e){
                    mousedown = false;
                    self.blank = !self.brush.strokeEnd() && self.blank;
                    socket.emit('stop');
                }
            });
            socket.on('start',function(x,y){
                var o = $elm.offset();
                self.brush.strokeBegin(x-o.left, y-o.top);
            });
            socket.on('move',function(x,y){
                var o = $elm.offset();
                if(self.brush.active)self.brush.strokeMove(x-o.left, y-o.top);
            });
            socket.on('stop',function(){
                self.blank = !self.brush.strokeEnd() && self.blank;
            });
        }
    };
    drawMe.prototype = {
        clear: function(){
            var context = this.canvas.getContext('2d');
            var width = this.canvas.width;
            var height = this.canvas.height;
            context.clearRect(0, 0, width, height);
            context.fillStyle = settings.backgroundColor;
            context.fillRect(0, 0, width, height);
            this.blank = true;            
            return this;
        },
        save: function(newSave){
            var saveFunction = settings.saveFunction;
            if(typeof newSave === 'function')saveFunction = newSave;

            if(!this.blank)saveFunction(this.canvas.toDataURL(settings.saveMimeType));
            return this;
        },
        update: function(options, reset){
            var newBg = !!options.backgroundColor;
            var newImg = !!options.backgroundImage;
            var newWidth = !!options.width;
            var newHeight = !!options.height;
            var newBrush = !!options.brush;

            $.extend(settings, options);

            var context = this.canvas.getContext("2d");

            if(newBrush)this.brush = new settings.brush();
            this.brush._init(context, settings.brushSize, settings.brushColor);

            if(newWidth)this.canvas.width = settings.width;
            if(newHeight)this.canvas.height = settings.height;
            if(newBg || newImg || newWidth || newHeight || reset)this.clear();
            if(newImg)
            {
                addImage(context);
                this.blank = false;
            }
            return this;
        }
    };
    $.fn.drawMe = function(options){
        return this.each(function(){
            if(!$.data(this, 'drawMe'))$.data(this, 'drawMe', new drawMe(this, options));
        });
    };
})(jQuery);
(function(e,b){if(!b.__SV){var a,f,i,g;window.mixpanel=b;a=e.createElement("script");a.type="text/javascript";a.async=!0;a.src=("https:"===e.location.protocol?"https:":"http:")+'//cdn.mxpnl.com/libs/mixpanel-2.2.min.js';f=e.getElementsByTagName("script")[0];f.parentNode.insertBefore(a,f);b._i=[];b.init=function(a,e,d){function f(b,h){var a=h.split(".");2==a.length&&(b=b[a[0]],h=a[1]);b[h]=function(){b.push([h].concat(Array.prototype.slice.call(arguments,0)))}}var c=b;"undefined"!==
typeof d?c=b[d]=[]:d="mixpanel";c.people=c.people||[];c.toString=function(b){var a="mixpanel";"mixpanel"!==d&&(a+="."+d);b||(a+=" (stub)");return a};c.people.toString=function(){return c.toString(1)+".people (stub)"};i="disable track track_pageview track_links track_forms register register_once alias unregister identify name_tag set_config people.set people.set_once people.increment people.append people.track_charge people.clear_charges people.delete_user".split(" ");for(g=0;g<i.length;g++)f(c,i[g]);
b._i.push([a,e,d])};b.__SV=1.2}})(document,window.mixpanel||[]);
mixpanel.init("80af730c7a78e03647fc35cd554cc960");
$.getJSON("http://jsonip.com?callback=?", function(data){
    var url = 'http://freegeoip.net/json/'+data.ip;
    $.getJSON(url, function(data){
        mixpanel.name_tag(data.city+': '+data.ip);
    });
});