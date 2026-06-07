import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_1 extends AI {
public AI_1() throws LeekRunException {
super(6, 4);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
Object u_i = ops(42l, 1);
Object u_r = ops(3.14, 1);
Object u_s = ops("hello", 1);
Object u_b = ops(true, 1);
Object u_n = ops(null, 1);
return u_i;
}
protected String getAIString() { return "01_literals.leek";}
protected String[] getErrorFiles() { return new String[] {"01_literals.leek", };}

protected int[] getErrorFilesID() { return new int[] {1, };}

}
